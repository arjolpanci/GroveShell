//! Generates a multi-resolution `.ico` from `media/logo.png` at build time
//! and embeds it as this exe's resource ID 1 (both the file's own icon and,
//! loaded at runtime via `LoadImageW(.., PCWSTR(1 as _), ..)`, the tray
//! icon and settings-window title-bar icon). Generating this at build time
//! rather than committing a binary `.ico` avoids needing an external
//! image-conversion tool in this repo's toolchain.

use std::path::Path;

const ICON_SIZES: &[u32] = &[16, 32, 48, 256];

fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    let logo_path = Path::new(&manifest_dir).join("../../media/logo.png");
    println!("cargo:rerun-if-changed={}", logo_path.display());

    let source = image::open(&logo_path)
        .unwrap_or_else(|e| panic!("failed to open {}: {e}", logo_path.display()));

    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in ICON_SIZES {
        let resized = source.resize_exact(size, size, image::imageops::FilterType::Lanczos3);
        let rgba = resized.to_rgba8();
        let image = ico::IconImage::from_rgba_data(size, size, rgba.into_raw());
        let entry = ico::IconDirEntry::encode(&image)
            .unwrap_or_else(|e| panic!("failed to encode {size}x{size} icon: {e}"));
        icon_dir.add_entry(entry);
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set");
    let ico_path = Path::new(&out_dir).join("icon.ico");
    let ico_file = std::fs::File::create(&ico_path)
        .unwrap_or_else(|e| panic!("failed to create {}: {e}", ico_path.display()));
    icon_dir
        .write(ico_file)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", ico_path.display()));

    let rc_path = Path::new(&out_dir).join("icon.rc");
    let ico_path_str = ico_path.display().to_string().replace('\\', "/");
    std::fs::write(&rc_path, format!("1 ICON \"{}\"\n", ico_path_str))
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", rc_path.display()));

    embed_resource::compile(&rc_path, embed_resource::NONE);
}
