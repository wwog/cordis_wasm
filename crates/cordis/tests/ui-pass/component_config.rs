use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct Config {
    label: String,
}

#[cordis::component(name = "configured", config = Config)]
struct Configured;

#[cordis::component_impl]
impl Configured {
    #[cordis::apply]
    async fn start(
        &mut self,
        _context: cordis::ComponentContext<ConfiguredDependencies>,
        config: Config,
    ) -> Result<(), cordis::CordisError> {
        let _ = config.label;
        Ok(())
    }
}

fn main() {
    use cordis::ComponentDefinition;

    let _ = Configured::descriptor();
}
