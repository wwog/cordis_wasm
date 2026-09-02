#[cordis::component]
struct ConsumingApply;

#[cordis::component_impl]
impl ConsumingApply {
    #[cordis::apply]
    async fn start(
        self,
        _context: cordis::ComponentContext<ConsumingApplyDependencies>,
        _config: (),
    ) -> Result<(), cordis::CordisError> {
        Ok(())
    }
}

fn main() {}
