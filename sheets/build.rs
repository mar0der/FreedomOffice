use std::io::Write;
use std::path::PathBuf;

include!("src/icon.rs");

/// Writes a multi-size .ico (32-bit BMP entries) from the procedural icon.
fn write_ico(path: &std::path::Path) {
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let mut images = Vec::new();
    for &sz in &sizes {
        let rgba = icon_rgba(sz);
        let mut bmp = Vec::new();
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&(sz as i32).to_le_bytes());
        bmp.extend_from_slice(&((sz * 2) as i32).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes());
        bmp.extend_from_slice(&[0u8; 24]);
        for y in (0..sz).rev() {
            for x in 0..sz {
                let i = ((y * sz + x) * 4) as usize;
                bmp.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
            }
        }
        let mask_row = sz.div_ceil(32) * 4;
        bmp.extend(std::iter::repeat_n(0u8, (mask_row * sz) as usize));
        images.push((sz, bmp));
    }
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(&[0, 0, 1, 0]).unwrap();
    f.write_all(&(images.len() as u16).to_le_bytes()).unwrap();
    let mut offset = 6 + 16 * images.len() as u32;
    for (sz, data) in &images {
        let d = if *sz >= 256 { 0u8 } else { *sz as u8 };
        f.write_all(&[d, d, 0, 0]).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&32u16.to_le_bytes()).unwrap();
        f.write_all(&(data.len() as u32).to_le_bytes()).unwrap();
        f.write_all(&offset.to_le_bytes()).unwrap();
        offset += data.len() as u32;
    }
    for (_, data) in &images {
        f.write_all(data).unwrap();
    }
}

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out.join("app.ico");
    write_ico(&ico);
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets").join("app.manifest");
    let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let v: Vec<&str> = version.split('.').collect();
    let rc = format!(
        r#"1 ICON "{ico}"
1 24 "{manifest}"

1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "FreedomOffice Sheets"
      VALUE "ProductName", "FreedomOffice Sheets"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "LegalCopyright", "MIT License"
      VALUE "OriginalFilename", "FreedomSheets.exe"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = esc(&ico),
        manifest = esc(&manifest),
        a = v[0],
        b = v[1],
        c = v[2],
    );
    let rc_path = out.join("app.rc");
    std::fs::write(&rc_path, rc).unwrap();
    println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
    // Large main-thread stack for deep formula dependency chains.
    println!("cargo:rustc-link-arg-bins=/STACK:268435456");
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_required().unwrap();
}
