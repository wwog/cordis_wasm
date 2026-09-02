#[cordis::service]
trait Clock {
    async fn now(&self) -> Result<u64, String>;
}

#[cordis::component]
#[cordis::inject(Clock)]
#[cordis::inject(Clock)]
struct DuplicateInject;

fn main() {}
