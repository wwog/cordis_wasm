#[cordis::event(mode = "waterfall")]
trait MismatchedWaterfall {
    type Input = String;
    type Output = u64;
}

fn main() {}
