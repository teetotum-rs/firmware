//! Tells cargo that the name comes from the environment, so that building the same sources
//! under a second name really builds a second module instead of handing back the first.
fn main() {
    println!("cargo::rerun-if-env-changed=TEETOTUM_DUMMY_NAME");
}
