fn main() {
    // `bk update` fetches the release archive made for the platform this bk was built for.
    println!("cargo:rustc-env=BK_TARGET={}", std::env::var("TARGET").expect("cargo sets TARGET"));
}
