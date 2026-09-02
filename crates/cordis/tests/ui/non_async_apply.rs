#[cordis::component]
struct NonAsync;

#[cordis::component_impl]
impl NonAsync {
    #[cordis::apply]
    fn start(
        self,
        _context: cordis::ComponentContext<NonAsyncDependencies>,
        _config: (),
    ) -> Result<(), cordis::CordisError> {
        Ok(())
    }
}

fn main() {}
