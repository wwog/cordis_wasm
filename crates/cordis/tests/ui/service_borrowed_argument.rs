#[cordis::service]
trait BorrowedArgument {
    async fn send(&self, value: &str) -> Result<(), String>;
}

fn main() {}
