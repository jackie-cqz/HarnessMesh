//! macOS: embed an Info.plist in the hmesh executable. The system reads it
//! to ask for Local Network access (TN3179), which a daemon started at login
//! needs to reach other computers on the LAN and to find them over Bonjour;
//! without a usage description it may refuse without asking.
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>dev.harnessmesh.hmesh</string>
	<key>CFBundleName</key>
	<string>hmesh</string>
	<key>CFBundleDisplayName</key>
	<string>HarnessMesh</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>NSLocalNetworkUsageDescription</key>
	<string>HarnessMesh connects to your other computers on this network, lists the ones ready to pair, and finds the ones paired with it when their addresses change.</string>
	<key>NSBonjourServices</key>
	<array>
		<string>_hmesh._tcp</string>
		<string>_hmesh-find._tcp</string>
	</array>
</dict>
</plist>
"#
    );
    let path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("Info.plist");
    fs::write(&path, plist).expect("write Info.plist");
    println!(
        "cargo:rustc-link-arg-bin=hmesh=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        path.display()
    );
}
