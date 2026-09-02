#[cordis::service]
trait NonResult {
    async fn read(&self) -> u64;
}

fn main() {}
