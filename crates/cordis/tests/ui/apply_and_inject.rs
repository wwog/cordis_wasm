#[cordis::service]
trait Clock {
    async fn now(&self) -> Result<u64, String>;
}

#[cordis::component]
struct ApplyAndInject;

#[cordis::component_impl]
impl ApplyAndInject {
    #[cordis::apply]
    #[cordis::inject(Clock)]
    async fn start(
        &mut self,
        _context: cordis::ComponentContext<ApplyAndInjectDependencies>,
        _config: (),
    ) -> Result<(), cordis::CordisError> {
        Ok(())
    }
}

fn main() {}
