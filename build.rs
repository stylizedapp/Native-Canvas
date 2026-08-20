fn main() {
    // Увеличенный стек главного потока (защита от переполнения стека в release
    // с panic=abort; Windows при этом падал бы с 0xc0000409).
    // 64 МБ резервируется, коммитится по мере использования.
    println!("cargo:rustc-link-arg=/STACK:67108864");
    slint_build::compile("ui/main.slint").expect("Slint build failed");
}