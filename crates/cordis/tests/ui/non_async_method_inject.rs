#[cordis::service]
trait Clock {
    async fn now(&self) -> Result<u64, String>;
}

#[cordis::component]
struct NonAsyncMethodInject;

#[cordis::component_impl]
impl NonAsyncMethodInject {
    #[cordis::apply]
    async fn start(
        &mut self,
        _context: cordis::ComponentContext<NonAsyncMethodInjectDependencies>,
        _config: (),
    ) -> Result<(), cordis::CordisError> {
        Ok(())
    }

    #[cordis::inject(Clock)]
    fn bind_clock(
        &mut self,
        _context: cordis::MethodContext<NonAsyncMethodInjectBindClockDependencies>,
    ) -> Result<(), cordis::CordisError> {
        Ok(())
    }
}

fn main() {}
