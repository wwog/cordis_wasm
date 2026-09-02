#[cordis::event(mode = "broadcast")]
trait InvalidMode {
    type Input = String;
    type Output = String;
}

fn main() {}
