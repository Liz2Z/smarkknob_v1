fn main() {
    wifi_config();
    linker_be_nice();
    println!("cargo:rustc-link-arg-tests=-Tembedded-test.x");
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

fn wifi_config() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=.local/wifi.env");
    // Plain KEY=value data, never evaluated by a shell. Environment overrides
    // the ignored local file; empty defaults allow credential-free CI builds.
    let local = std::fs::read_to_string(".local/wifi.env").unwrap_or_default();
    let mut generated = String::new();
    for (name, variable) in [
        ("SSID", "MOTOR_WIFI_SSID"),
        ("PASSWORD", "MOTOR_WIFI_PASSWORD"),
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
        let value = std::env::var(variable).unwrap_or_else(|_| {
            local
                .lines()
                .filter_map(|line| line.split_once('='))
                .find_map(|(key, value)| (key.trim() == variable).then_some(value.to_owned()))
                .unwrap_or_default()
        });
        let literal = format!("{value:?}");
        generated.push_str(&format!("pub const WIFI_{name}: &str = {literal};\n"));
    }
    let path =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("wifi_config.rs");
    std::fs::write(path, generated).expect("write private build-time Wi-Fi config");
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
