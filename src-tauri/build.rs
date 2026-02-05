use std::path::PathBuf;

fn main() {
	#[cfg(target_os = "macos")]
	{
		const OBJC_FILE: &str = "macos/smjobbless.m";
		println!("cargo:rerun-if-changed={}", OBJC_FILE);

		cc::Build::new()
			.file(OBJC_FILE)
			.flag("-fobjc-arc")
			.compile("ultunnel_smjobbless");
		println!("cargo:rustc-link-lib=framework=Foundation");
		println!("cargo:rustc-link-lib=framework=ServiceManagement");
		println!("cargo:rustc-link-lib=framework=Security");

		// чтобы убрать warning unexpected cfg(mobile)
		println!("cargo:rustc-check-cfg=cfg(mobile)");
		println!("cargo:rustc-check-cfg=cfg(desktop)");
		println!("cargo:rustc-cfg=desktop");

		println!("cargo:rerun-if-changed=Info.plist");
		let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
		let plist: PathBuf = [manifest_dir.as_str(), "Info.plist"].iter().collect();
		println!("cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
	}

	let attrs = {
		let mut attrs = tauri_build::Attributes::new();

		#[cfg(target_os = "windows")]
		{
			let mut windows = tauri_build::WindowsAttributes::new();
			windows = windows.app_manifest(include_str!("manifest.xml").to_string());
			attrs = attrs.windows_attributes(windows);
		}

		attrs
	};

	tauri_build::try_build(attrs).expect("tauri build failed");
}
