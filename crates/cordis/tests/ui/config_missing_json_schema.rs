use serde::Deserialize;

#[derive(Deserialize)]
struct Config;

#[cordis::component(config = Config)]
struct MissingJsonSchema;

fn main() {}
