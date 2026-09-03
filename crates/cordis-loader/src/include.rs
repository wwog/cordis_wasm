use crate::{EntryId, EntrySpec, LoaderError};
use rhai::{Engine, Scope};
use serde::Deserialize;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use serde_saphyr::Tagged;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncludeFormat {
    Json,
    Yaml,
}

impl IncludeFormat {
    /// Infers JSON or YAML from a file extension.
    ///
    /// # Errors
    ///
    /// Returns an include error for unsupported extensions.
    pub fn from_path(path: &Path) -> Result<Self, LoaderError> {
        match path.extension().and_then(|value| value.to_str()) {
            Some("json") => Ok(Self::Json),
            Some("yaml" | "yml") => Ok(Self::Yaml),
            _ => Err(LoaderError::Include(format!(
                "unsupported include format for {}",
                path.display()
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Patch {
    pub target: Option<EntryId>,
    pub action: PatchAction,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatchAction {
    Merge(Value),
    Replace(EntrySpec),
    Remove,
    Insert { index: usize, entry: EntrySpec },
}

#[derive(Debug)]
pub struct IncludeDocument {
    path: PathBuf,
    format: IncludeFormat,
    readonly: bool,
    entries: Vec<EntrySpec>,
}

impl IncludeDocument {
    /// Loads, evaluates, and patches an include file.
    ///
    /// # Errors
    ///
    /// Returns filesystem, syntax, expression, or patch errors.
    pub fn load(
        path: impl Into<PathBuf>,
        patches: &[Patch],
        expression_context: &Value,
    ) -> Result<Self, LoaderError> {
        let path = path.into();
        let format = IncludeFormat::from_path(&path)?;
        let source = fs::read_to_string(&path).map_err(include_error)?;
        let readonly = is_readonly(&path).map_err(include_error)?;
        Self::from_source(path, format, readonly, &source, patches, expression_context)
    }

    /// Parses include source with an explicit format and write policy.
    ///
    /// # Errors
    ///
    /// Returns syntax, expression, or patch errors.
    pub fn from_source(
        path: impl Into<PathBuf>,
        format: IncludeFormat,
        readonly: bool,
        source: &str,
        patches: &[Patch],
        expression_context: &Value,
    ) -> Result<Self, LoaderError> {
        let value = match format {
            IncludeFormat::Json => serde_json::from_str(source)
                .map_err(|error| LoaderError::Include(error.to_string()))?,
            IncludeFormat::Yaml => parse_yaml(source, expression_context)?,
        };
        let mut entries = decode_entries(value)?;
        apply_patches(&mut entries, patches)?;
        Ok(Self {
            path: path.into(),
            format,
            readonly,
            entries,
        })
    }

    pub fn entries(&self) -> &[EntrySpec] {
        &self.entries
    }

    pub fn entries_mut(&mut self) -> &mut Vec<EntrySpec> {
        &mut self.entries
    }

    pub fn readonly(&self) -> bool {
        self.readonly
    }

    /// Atomically writes the materialized Entry array back to its source.
    ///
    /// # Errors
    ///
    /// Returns an error for read-only documents or failed filesystem operations.
    pub fn write_back(&self) -> Result<(), LoaderError> {
        if self.readonly {
            return Err(LoaderError::Include(format!(
                "include {} is read-only",
                self.path.display()
            )));
        }
        let bytes = match self.format {
            IncludeFormat::Json => serde_json::to_vec_pretty(&self.entries)
                .map_err(|error| LoaderError::Include(error.to_string()))?,
            IncludeFormat::Yaml => serde_saphyr::to_string(&self.entries)
                .map_err(|error| LoaderError::Include(error.to_string()))?
                .into_bytes(),
        };
        atomic_write(&self.path, &bytes).map_err(include_error)
    }
}

pub struct ExprEvaluator {
    engine: Engine,
}

impl std::fmt::Debug for ExprEvaluator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExprEvaluator")
            .finish_non_exhaustive()
    }
}

impl Default for ExprEvaluator {
    fn default() -> Self {
        let mut engine = Engine::new();
        engine
            .set_max_operations(10_000)
            .set_max_expr_depths(32, 16)
            .set_max_string_size(64 * 1024)
            .set_max_array_size(10_000)
            .set_max_map_size(10_000);
        for symbol in [
            "eval", "import", "export", "fn", "while", "loop", "for", "try", "throw",
        ] {
            engine.disable_symbol(symbol);
        }
        Self { engine }
    }
}

impl ExprEvaluator {
    /// Evaluates one limited Rhai expression against the JSON `ctx` snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for forbidden syntax, budget exhaustion, or non-JSON results.
    pub fn evaluate(&self, expression: &str, context: &Value) -> Result<Value, LoaderError> {
        let dynamic = rhai::serde::to_dynamic(context).map_err(|error| {
            LoaderError::Include(format!("invalid expression context: {error}"))
        })?;
        let mut scope = Scope::new();
        scope.push_dynamic("ctx", dynamic);
        let value = self
            .engine
            .eval_expression_with_scope::<rhai::Dynamic>(&mut scope, expression)
            .map_err(|error| {
                LoaderError::Include(format!("!expr `{expression}` failed: {error}"))
            })?;
        rhai::serde::from_dynamic(&value)
            .map_err(|error| LoaderError::Include(format!("!expr result is not JSON: {error}")))
    }
}

#[derive(Debug)]
enum YamlNode {
    Null(()),
    Bool(bool),
    Integer(i64),
    Unsigned(u64),
    Float(f64),
    String(String),
    Sequence(Vec<Tagged<YamlNode>>),
    Mapping(BTreeMap<String, Tagged<YamlNode>>),
}

impl<'de> Deserialize<'de> for YamlNode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct YamlNodeVisitor;

        impl<'de> Visitor<'de> for YamlNodeVisitor {
            type Value = YamlNode;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a YAML value")
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(YamlNode::Null(()))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(YamlNode::Null(()))
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(YamlNode::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(YamlNode::Integer(value))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(YamlNode::Unsigned(value))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
                Ok(YamlNode::Float(value))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(YamlNode::String(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(YamlNode::String(value))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(YamlNode::Sequence(values))
            }

            fn visit_map<A>(self, mut mapping: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = mapping.next_entry()? {
                    values.insert(key, value);
                }
                Ok(YamlNode::Mapping(values))
            }
        }

        deserializer.deserialize_any(YamlNodeVisitor)
    }
}

fn parse_yaml(source: &str, context: &Value) -> Result<Value, LoaderError> {
    let root: Tagged<YamlNode> =
        serde_saphyr::from_str(source).map_err(|error| LoaderError::Include(error.to_string()))?;
    tagged_to_json(root, &ExprEvaluator::default(), context)
}

fn tagged_to_json(
    tagged: Tagged<YamlNode>,
    evaluator: &ExprEvaluator,
    context: &Value,
) -> Result<Value, LoaderError> {
    let Tagged(node, tag) = tagged;
    if let Some(tag) = tag {
        if tag != "!expr" {
            return Err(LoaderError::Include(format!(
                "unsupported YAML tag `{tag}`"
            )));
        }
        return match node {
            YamlNode::String(expression) => evaluator.evaluate(&expression, context),
            _ => Err(LoaderError::Include(
                "!expr value must be a scalar expression".into(),
            )),
        };
    }
    match node {
        YamlNode::Null(()) => Ok(Value::Null),
        YamlNode::Bool(value) => Ok(Value::Bool(value)),
        YamlNode::Integer(value) => Ok(Value::Number(value.into())),
        YamlNode::Unsigned(value) => Ok(Value::Number(value.into())),
        YamlNode::Float(value) => Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| LoaderError::Include("non-finite YAML number".into())),
        YamlNode::String(value) => Ok(Value::String(value)),
        YamlNode::Sequence(values) => values
            .into_iter()
            .map(|value| tagged_to_json(value, evaluator, context))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        YamlNode::Mapping(values) => values
            .into_iter()
            .map(|(key, value)| Ok((key, tagged_to_json(value, evaluator, context)?)))
            .collect::<Result<Map<_, _>, LoaderError>>()
            .map(Value::Object),
    }
}

fn decode_entries(value: Value) -> Result<Vec<EntrySpec>, LoaderError> {
    if value.is_array() {
        serde_json::from_value(value).map_err(|error| LoaderError::Include(error.to_string()))
    } else if let Some(entries) = value.get("entries") {
        serde_json::from_value(entries.clone())
            .map_err(|error| LoaderError::Include(error.to_string()))
    } else {
        Err(LoaderError::Include(
            "include root must be an entry array or contain `entries`".into(),
        ))
    }
}

fn apply_patches(entries: &mut Vec<EntrySpec>, patches: &[Patch]) -> Result<(), LoaderError> {
    for patch in patches {
        match &patch.action {
            PatchAction::Insert { index, entry } => {
                let target = if let Some(parent) = &patch.target {
                    let group = find_mut(entries, parent)
                        .ok_or_else(|| LoaderError::MissingEntry(parent.clone()))?;
                    if !group.group {
                        return Err(LoaderError::ParentNotGroup(parent.clone()));
                    }
                    &mut group.children
                } else {
                    &mut *entries
                };
                target.insert((*index).min(target.len()), entry.clone());
            }
            PatchAction::Remove => {
                let target = patch
                    .target
                    .as_ref()
                    .ok_or_else(|| LoaderError::Include("remove patch requires target".into()))?;
                remove_entry(entries, target)
                    .ok_or_else(|| LoaderError::MissingEntry(target.clone()))?;
            }
            PatchAction::Replace(replacement) => {
                let target = patch
                    .target
                    .as_ref()
                    .ok_or_else(|| LoaderError::Include("replace patch requires target".into()))?;
                if &replacement.id != target {
                    return Err(LoaderError::Include(format!(
                        "replacement id `{}` does not match target `{target}`",
                        replacement.id
                    )));
                }
                *find_mut(entries, target)
                    .ok_or_else(|| LoaderError::MissingEntry(target.clone()))? =
                    replacement.clone();
            }
            PatchAction::Merge(value) => {
                let target = patch
                    .target
                    .as_ref()
                    .ok_or_else(|| LoaderError::Include("merge patch requires target".into()))?;
                let entry = find_mut(entries, target)
                    .ok_or_else(|| LoaderError::MissingEntry(target.clone()))?;
                let mut base = serde_json::to_value(&*entry)
                    .map_err(|error| LoaderError::Include(error.to_string()))?;
                deep_merge(&mut base, value.clone());
                let merged: EntrySpec = serde_json::from_value(base)
                    .map_err(|error| LoaderError::Include(error.to_string()))?;
                if &merged.id != target {
                    return Err(LoaderError::Include(format!(
                        "merged id `{}` does not match target `{target}`",
                        merged.id
                    )));
                }
                *entry = merged;
            }
        }
    }
    Ok(())
}

fn deep_merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                deep_merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, patch) => *base = patch,
    }
}

fn find_mut<'a>(entries: &'a mut [EntrySpec], id: &EntryId) -> Option<&'a mut EntrySpec> {
    for entry in entries {
        if &entry.id == id {
            return Some(entry);
        }
        if let Some(found) = find_mut(&mut entry.children, id) {
            return Some(found);
        }
    }
    None
}

fn remove_entry(entries: &mut Vec<EntrySpec>, id: &EntryId) -> Option<EntrySpec> {
    if let Some(index) = entries.iter().position(|entry| &entry.id == id) {
        return Some(entries.remove(index));
    }
    for entry in entries {
        if let Some(found) = remove_entry(&mut entry.children, id) {
            return Some(found);
        }
    }
    None
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("include");
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn is_readonly(path: &Path) -> std::io::Result<bool> {
    let metadata = fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(metadata.permissions().mode() & 0o222 == 0)
    }
    #[cfg(not(unix))]
    {
        Ok(metadata.permissions().readonly())
    }
}

#[allow(clippy::needless_pass_by_value)]
fn include_error(error: std::io::Error) -> LoaderError {
    LoaderError::Include(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_expr_is_evaluated_recursively_in_restricted_scope() {
        let document = IncludeDocument::from_source(
            "config.yaml",
            IncludeFormat::Yaml,
            false,
            "- id: app\n  component: builtin:test\n  config:\n    port: !expr ctx.port + 1\n",
            &[],
            &serde_json::json!({"port": 40}),
        )
        .unwrap();
        assert_eq!(document.entries()[0].config["port"], 41);
        assert!(
            ExprEvaluator::default()
                .evaluate("eval(\"40 + 2\")", &Value::Null)
                .is_err()
        );

        let ordinary = parse_yaml("expr: literal", &Value::Null).unwrap();
        assert_eq!(ordinary, serde_json::json!({"expr":"literal"}));
        assert!(parse_yaml("value: !danger nope", &Value::Null).is_err());
    }

    #[test]
    fn patches_support_merge_group_insert_and_name_mismatch_detection() {
        let child = EntrySpec::leaf("child", "builtin:child").unwrap();
        let patches = [
            Patch {
                target: Some(EntryId::new("app").unwrap()),
                action: PatchAction::Merge(serde_json::json!({"config":{"value":2}})),
            },
            Patch {
                target: Some(EntryId::new("group").unwrap()),
                action: PatchAction::Insert {
                    index: 0,
                    entry: child,
                },
            },
        ];
        let document = IncludeDocument::from_source(
            "config.json",
            IncludeFormat::Json,
            false,
            r#"[{"id":"app","component":"builtin:app","config":{"value":1}},{"id":"group","group":true}]"#,
            &patches,
            &Value::Null,
        )
        .unwrap();
        assert_eq!(document.entries()[0].config["value"], 2);
        assert_eq!(document.entries()[1].children[0].id.as_str(), "child");

        let mismatch = Patch {
            target: Some(EntryId::new("app").unwrap()),
            action: PatchAction::Merge(serde_json::json!({"id":"other"})),
        };
        assert!(
            IncludeDocument::from_source(
                "config.json",
                IncludeFormat::Json,
                false,
                r#"[{"id":"app","component":"builtin:app"}]"#,
                &[mismatch],
                &Value::Null,
            )
            .is_err()
        );
    }

    #[test]
    fn write_back_is_atomic_and_readonly_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("entries.json");
        fs::write(&path, r#"[{"id":"app","component":"builtin:app"}]"#).unwrap();
        let mut document = IncludeDocument::load(&path, &[], &Value::Null).unwrap();
        document.entries_mut()[0].config = serde_json::json!({"updated":true});
        document.write_back().unwrap();
        let loaded = IncludeDocument::load(&path, &[], &Value::Null).unwrap();
        assert_eq!(loaded.entries()[0].config["updated"], true);

        let readonly = IncludeDocument::from_source(
            &path,
            IncludeFormat::Json,
            true,
            r#"[{"id":"app","component":"builtin:app"}]"#,
            &[],
            &Value::Null,
        )
        .unwrap();
        assert!(readonly.write_back().is_err());
    }

    #[test]
    fn missing_patch_target_is_an_error() {
        let patch = Patch {
            target: Some(EntryId::new("missing").unwrap()),
            action: PatchAction::Remove,
        };
        assert!(matches!(
            IncludeDocument::from_source(
                "config.json",
                IncludeFormat::Json,
                false,
                "[]",
                &[patch],
                &Value::Null,
            ),
            Err(LoaderError::MissingEntry(_))
        ));
    }
}
