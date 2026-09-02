use std::rc::Rc;

#[cordis::component]
struct NonSendApply;

#[cordis::component_impl]
impl NonSendApply {
    #[cordis::apply]
    async fn start(
        &mut self,
        _context: cordis::ComponentContext<NonSendApplyDependencies>,
        _config: (),
    ) -> Result<(), cordis::CordisError> {
        let value = Rc::new(1_u8);
        std::future::pending::<()>().await;
        drop(value);
        Ok(())
    }
}

fn main() {}
