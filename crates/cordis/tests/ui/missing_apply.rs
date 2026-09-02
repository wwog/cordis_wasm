#[cordis::component]
struct MissingApply;

#[cordis::component_impl]
impl MissingApply {
    async fn start(self) {}
}

fn main() {}
