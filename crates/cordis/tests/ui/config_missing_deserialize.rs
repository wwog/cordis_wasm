use schemars::JsonSchema;

#[derive(JsonSchema)]
struct Config;

#[cordis::component(config = Config)]
struct MissingDeserialize;

fn main() {}
