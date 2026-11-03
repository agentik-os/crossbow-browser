//! `cargo xtask icon` renders the app icon; `cargo xtask bundle [--release]`
//! builds Crossbow and assembles the platform bundle (macOS: Crossbow.app with
//! the Chromium framework and helper apps). `cargo xtask blocking-fixture`
//! serves a page on this Mac for trying content blocking by hand; `cargo xtask
//! update-lists` replaces the bundled filter lists, by hand only. `cargo xtask
//! public-export` writes a sanitized snapshot of one revision for the public
//! repository (see public_export.rs); `cargo xtask public-parity` regenerates
//! its Arc Settings parity page from the roadmap. `cargo xtask release-key`
//! makes an update-signing key pair and `cargo xtask release-sign` writes the
//! signed update feed for one archive (see release_signing.rs).

mod blocking_fixture;
mod public_export;
mod public_parity;
#[cfg(unix)]
mod release_signing;
mod update_lists;

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let release = args.iter().any(|a| a == "--release");
    let result = match args.first().map(String::as_str) {
        Some("icon") => icon(),
        Some("bundle") => bundle(release, &args),
        Some("blocking-fixture") => blocking_fixture::run(&args[1..], &root()),
        Some("public-export") => public_export::run(&args[1..], &root()),
        Some("public-parity") => public_parity::run(&args[1..], &root()),
        Some("update-lists") => update_lists::run(&args[1..], &root()),
        #[cfg(unix)]
        Some("release-key") => release_signing::key(&args[1..], &root()),
        #[cfg(unix)]
        Some("release-sign") => release_signing::sign(&args[1..]),
        _ => Err("usage: cargo xtask <icon | bundle [--release] [--candidate NAME] | blocking-fixture [--port N] [--lists DIR] | public-export --rev REV --out DIR [--commit-into TREE] [--config FILE] | public-parity | update-lists [--from URL] | release-key --out DIR | release-sign --archive FILE --version X.Y.Z --notes FILE --key FILE --url URL --team TEAMID [--valid-days N] --out FILE>".into()),
    };
    if let Err(e) = result {
        eprintln!("xtask: {e}");
        std::process::exit(1);
    }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("{cmd:?}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{cmd:?} failed with {status}")) }
}

// -- icon -----------------------------------------------------------------------

/// Flat, monochrome bodies; all intermediate shades come from edge antialiasing.
#[derive(Clone, Copy)]
enum IconStyle { Black, Graphite, Pearl }

fn render_icon(size: u32) -> tiny_skia::Pixmap {
    render_icon_style(size, IconStyle::Black)
}

fn render_icon_style(size: u32, style: IconStyle) -> tiny_skia::Pixmap {
    use tiny_skia::*;
    let s = size as f32;
    let mut pm = Pixmap::new(size, size).unwrap();
    // Apple's grid: an 824/1024 body with a ~185 radius.
    let inset = s * 100.0 / 1024.0;
    let body = s - 2.0 * inset;
    let r = body * 0.225;
    let rect = Rect::from_xywh(inset, inset, body, body).unwrap();
    let mut pb = PathBuilder::new();
    let (x0, y0, x1, y1) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let k = 0.5523 * r;
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.cubic_to(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.cubic_to(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0);
    pb.close();
    let squircle = pb.finish().unwrap();

    let (ground, edge, mark) = match style {
        IconStyle::Black => (0, 48, 255),
        IconStyle::Graphite => (38, 76, 255),
        IconStyle::Pearl => (248, 210, 12),
    };
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.set_color_rgba8(ground, ground, ground, 255);
    pm.fill_path(&squircle, &paint, FillRule::Winding, Transform::identity(), None);

    // A fine, constant-tone rim separates the black body from a dark Dock.
    // No shading, glossy overlay, coloured glow or gradient shader.
    paint.set_color_rgba8(edge, edge, edge, 255);
    pm.stroke_path(&squircle, &paint, &Stroke { width: (s * 0.003).max(0.45), ..Default::default() }, Transform::identity(), None);

    // The mark, centred and drawn on a unit grid.
    let c = (s / 2.0, s / 2.0);
    let u = body / 2.0;
    let p = |x: f32, y: f32| (c.0 + x * u, c.1 + y * u);
    let mut white = Paint::default();
    white.set_color_rgba8(mark, mark, mark, 255);
    white.anti_alias = true;
    let stroke = |w: f32| Stroke { width: (w * u).max(0.8), line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };

    // Bow limb: an arc on the left.
    let mut bow = PathBuilder::new();
    let steps = 40;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let a = std::f32::consts::PI * (-0.3 + 0.6 * t);
        let (x, y) = p(-0.44 + 0.5 * a.cos(), 0.66 * a.sin());
        if i == 0 { bow.move_to(x, y) } else { bow.line_to(x, y) }
    }
    let bow = bow.finish().unwrap();
    pm.stroke_path(&bow, &white, &stroke(0.085), Transform::identity(), None);
    let a0 = std::f32::consts::PI * -0.3;
    let a1 = std::f32::consts::PI * 0.3;
    let top = p(-0.44 + 0.5 * a0.cos(), 0.66 * a0.sin());
    let bottom = p(-0.44 + 0.5 * a1.cos(), 0.66 * a1.sin());
    let nock = p(-0.5, 0.0);

    // String.
    let mut string = PathBuilder::new();
    string.move_to(top.0, top.1);
    string.line_to(nock.0, nock.1);
    string.line_to(bottom.0, bottom.1);
    pm.stroke_path(&string.finish().unwrap(), &white, &stroke(0.03), Transform::identity(), None);

    // Arrow shaft and head.
    let tip = p(0.52, 0.0);
    let mut shaft = PathBuilder::new();
    shaft.move_to(nock.0, nock.1);
    shaft.line_to(tip.0 - 0.06 * u, tip.1);
    pm.stroke_path(&shaft.finish().unwrap(), &white, &stroke(0.06), Transform::identity(), None);
    let mut head = PathBuilder::new();
    let (hx, hy) = p(0.66, 0.0);
    head.move_to(hx, hy);
    let (ax, ay) = p(0.42, -0.13);
    head.line_to(ax, ay);
    let (bx, by) = p(0.42, 0.13);
    head.line_to(bx, by);
    head.close();
    pm.fill_path(&head.finish().unwrap(), &white, FillRule::Winding, Transform::identity(), None);
    // Fletching.
    for dy in [-1.0f32, 1.0] {
        let mut f = PathBuilder::new();
        let (fx, fy) = p(-0.36, 0.0);
        f.move_to(fx, fy);
        let (gx, gy) = p(-0.48, 0.11 * dy);
        f.line_to(gx, gy);
        pm.stroke_path(&f.finish().unwrap(), &white, &stroke(0.045), Transform::identity(), None);
    }
    pm
}

fn icon() -> Result<(), String> {
    let res = root().join("app/resources");
    std::fs::create_dir_all(res.join("mac")).map_err(|e| e.to_string())?;
    render_icon(1024).save_png(res.join("icon-1024.png")).map_err(|e| e.to_string())?;
    render_icon(256).save_png(res.join("icon-256.png")).map_err(|e| e.to_string())?;
    for (name, style) in [("graphite", IconStyle::Graphite), ("pearl", IconStyle::Pearl)] {
        for size in [256, 1024] {
            render_icon_style(size, style).save_png(res.join(format!("icon-{name}-{size}.png"))).map_err(|e| e.to_string())?;
        }
    }
    println!("wrote the Black, Graphite and Pearl PNG icon assets");
    #[cfg(target_os = "macos")]
    {
        let set = std::env::temp_dir().join(format!("Crossbow-{}.iconset", std::process::id()));
        std::fs::create_dir(&set).map_err(|e| e.to_string())?;
        for (px, name) in [(16, "16x16"), (32, "16x16@2x"), (32, "32x32"), (64, "32x32@2x"), (128, "128x128"), (256, "128x128@2x"), (256, "256x256"), (512, "256x256@2x"), (512, "512x512"), (1024, "512x512@2x")] {
            render_icon(px).save_png(set.join(format!("icon_{name}.png"))).map_err(|e| e.to_string())?;
        }
        run(Command::new("iconutil").arg("-c").arg("icns").arg(&set).arg("-o").arg(res.join("mac/Crossbow.icns")))?;
        let _ = std::fs::remove_dir_all(&set);
        println!("wrote app/resources/mac/Crossbow.icns");
    }
    Ok(())
}

#[cfg(test)]
mod icon_tests {
    use super::*;

    #[test]
    fn default_has_monochrome_pixels_and_flat_black_body() {
        for size in [32, 64, 256, 1024] {
            let pm = render_icon(size);
            for pixel in pm.pixels() {
                assert_eq!(pixel.red(), pixel.green());
                assert_eq!(pixel.green(), pixel.blue());
            }
            // Samples through the body, away from the mark and antialiased rim.
            for x in [0.21, 0.24, 0.77, 0.80] {
                for y in [0.35, 0.41, 0.59, 0.65] {
                    let p = pm.pixel((size as f32 * x) as u32, (size as f32 * y) as u32).unwrap();
                    assert_eq!([p.red(), p.green(), p.blue(), p.alpha()], [0, 0, 0, 255]);
                }
            }
        }
    }

    #[test]
    fn mark_and_rim_remain_visible_at_launcher_and_dock_sizes() {
        for size in [16, 32, 64, 128, 256, 1024] {
            let pm = render_icon(size);
            assert_eq!(pm.pixel(0, 0).unwrap().alpha(), 0);
            let bright = pm.pixels().iter().filter(|p| p.alpha() == 255 && p.red() >= 160).count();
            assert!(bright > (size * size / 80) as usize, "missing mark at {size}px");
            let edge_y = (size as f32 * 100.0 / 1024.0).floor() as u32;
            let rim_visible = (edge_y.saturating_sub(1)..=edge_y + 1).any(|y| {
                let edge = pm.pixel(size / 2, y).unwrap();
                edge.alpha() > 0 && edge.red() > 0
            });
            assert!(rim_visible, "missing rim at {size}px");
            if size >= 64 {
                assert!(pm.pixels().iter().any(|p| p.red() == 255 && p.alpha() == 255));
            }
        }
    }

    #[test]
    fn built_in_variants_use_constant_neutral_body_colours() {
        for (style, tone) in [(IconStyle::Black, 0), (IconStyle::Graphite, 38), (IconStyle::Pearl, 248)] {
            let pm = render_icon_style(256, style);
            for y in 90..166 {
                let p = pm.pixel(56, y).unwrap();
                assert_eq!([p.red(), p.green(), p.blue(), p.alpha()], [tone, tone, tone, 255]);
            }
        }
    }
}

// -- bundle -----------------------------------------------------------------------

fn bundle(release: bool, args: &[String]) -> Result<(), String> {
    let root = root();
    let profile = if release { "release" } else { "debug" };
    let target = root.join("target").join(profile);
    let out = bundle_output(&target, args)?;
    let candidate = args.iter().any(|arg| arg == "--candidate");
    let server = bundle_server(&root, std::env::var("CROSSBOW_API_URL").ok(), candidate)?;
    let updater = bundle_updater(&root, |name| std::env::var(name).ok())?;
    let mut build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    build.current_dir(&root).args(["build", "-p", "crossbow", "--bins"]);
    match &server {
        Some(url) => build.env("CROSSBOW_API_URL", url),
        None => build.env_remove("CROSSBOW_API_URL"),
    };
    for (name, value) in UPDATER.into_iter().zip([&updater.url, &updater.keys, &updater.build]) {
        match value {
            Some(value) => build.env(name, value),
            None => build.env_remove(name),
        };
    }
    println!("sync server: {}", if server.is_some() { "set for this build" } else { "none (a source build)" });
    println!("updater: {}", if updater.url.is_some() { "set for this build" } else { "none" });
    if release {
        build.arg("--release");
    }
    run(&mut build)?;
    platform_bundle(&root, &target, &out, updater.build.as_deref())
}

/// The git-ignored file at the repository root that holds the settings below
/// for `bundle` when the environment doesn't, as `NAME=value` lines.
const BUILD_ENV: &str = ".crossbow-build.env";

/// The sync server, the update feed, its public keys and the build number.
const SETTINGS: [&str; 4] = ["CROSSBOW_API_URL", "CROSSBOW_UPDATE_URL", "CROSSBOW_UPDATE_KEYS", "CROSSBOW_BUILD_NUMBER"];

/// The settings that make a bundle's updater.
const UPDATER: [&str; 3] = ["CROSSBOW_UPDATE_URL", "CROSSBOW_UPDATE_KEYS", "CROSSBOW_BUILD_NUMBER"];

/// `name`: `env`, its value in the environment, else its line in
/// [`BUILD_ENV`]. Empty is unset.
fn setting(root: &Path, name: &str, env: Option<String>) -> Result<Option<String>, String> {
    if let Some(value) = env.filter(|value| !value.is_empty()) {
        return Ok(Some(value));
    }
    let path = root.join(BUILD_ENV);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut value = None;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        match line.split_once('=') {
            Some((key, found)) if SETTINGS.contains(&key) => {
                if key == name {
                    value = Some(found.trim().trim_matches('"').to_owned()).filter(|value| !value.is_empty());
                }
            }
            _ => return Err(format!("{BUILD_ENV} holds only {}=... lines and comments", SETTINGS.join("=..., "))),
        }
    }
    Ok(value)
}

/// The sync server a bundle is built for: `CROSSBOW_API_URL` from the
/// environment, else from [`BUILD_ENV`]. A `--candidate` bundle must have one;
/// the build itself refuses a value that isn't an https or loopback URL.
fn bundle_server(root: &Path, env: Option<String>, candidate: bool) -> Result<Option<String>, String> {
    let server = setting(root, "CROSSBOW_API_URL", env)?;
    if candidate && server.is_none() {
        return Err(format!("a --candidate bundle needs its sync server: set CROSSBOW_API_URL, or put CROSSBOW_API_URL=https://... in {BUILD_ENV}"));
    }
    Ok(server)
}

/// A bundle's updater: its feed and the public keys that sign it, both or
/// neither, and the build number its feed and `CFBundleVersion` name.
#[derive(Debug, Default, PartialEq)]
struct Updater {
    url: Option<String>,
    keys: Option<String>,
    build: Option<String>,
}

/// The updater a bundle is built with, each setting from `env` (the
/// environment), else from [`BUILD_ENV`]. The build itself refuses a feed
/// that isn't an https or loopback URL, and malformed keys.
fn bundle_updater(root: &Path, env: impl Fn(&str) -> Option<String>) -> Result<Updater, String> {
    let [url, keys, build] = UPDATER.map(|name| setting(root, name, env(name)));
    let updater = Updater { url: url?, keys: keys?, build: build? };
    if updater.url.is_some() != updater.keys.is_some() {
        return Err("an updater needs both CROSSBOW_UPDATE_URL and CROSSBOW_UPDATE_KEYS, or neither".into());
    }
    if updater.url.is_some() && updater.build.is_none() {
        return Err("a bundle with an updater needs CROSSBOW_BUILD_NUMBER: its feed and CFBundleVersion name it".into());
    }
    if updater.build.as_ref().is_some_and(|build| build.len() > 18 || !build.bytes().all(|b| b.is_ascii_digit())) {
        return Err("CROSSBOW_BUILD_NUMBER must be 1 to 18 digits".into());
    }
    Ok(updater)
}

#[cfg(test)]
mod bundle_server_tests {
    use super::*;

    fn root_with(name: &str, file: Option<&str>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-server-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(text) = file {
            std::fs::write(dir.join(BUILD_ENV), text).unwrap();
        }
        dir
    }

    #[test]
    fn the_environment_wins_then_the_build_file_then_none() {
        let file = root_with("file", Some("# The server for bundles.\nCROSSBOW_API_URL=\"https://sync.example.com\"\n"));
        let none = root_with("none", None);
        let env = Some("https://env.example.com".to_owned());
        assert_eq!(bundle_server(&file, env.clone(), true), Ok(Some("https://env.example.com".into())));
        assert_eq!(bundle_server(&file, None, true), Ok(Some("https://sync.example.com".into())));
        assert_eq!(bundle_server(&file, Some(String::new()), false), Ok(Some("https://sync.example.com".into())));
        assert_eq!(bundle_server(&none, None, false), Ok(None));
        assert_eq!(bundle_server(&none, env, false), Ok(Some("https://env.example.com".into())));
        for dir in [file, none] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn a_candidate_refuses_to_build_without_a_server() {
        let none = root_with("candidate", None);
        let empty = root_with("candidate-empty", Some("CROSSBOW_API_URL=\n"));
        for dir in [&none, &empty] {
            let error = bundle_server(dir, None, true).unwrap_err();
            assert!(error.starts_with("a --candidate bundle needs its sync server"), "{error}");
            assert_eq!(bundle_server(dir, None, false), Ok(None));
        }
        let other = root_with("candidate-other", Some("CROSSBOW_API_URL=https://sync.example.com\nOTHER=1\n"));
        assert!(bundle_server(&other, None, false).unwrap_err().contains("holds only CROSSBOW_API_URL"));
        for dir in [none, empty, other] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn the_updater_comes_whole_from_the_environment_or_the_build_file() {
        const KEY: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
        let file = root_with("updater", Some(&format!("CROSSBOW_API_URL=https://sync.example.com\nCROSSBOW_UPDATE_URL=https://updates.example.com/stable.json\nCROSSBOW_UPDATE_KEYS={KEY}\nCROSSBOW_BUILD_NUMBER=7\n")));
        let none = root_with("updater-none", None);
        let unset = |_: &str| None;
        let from_file = bundle_updater(&file, unset).unwrap();
        assert_eq!(from_file, Updater { url: Some("https://updates.example.com/stable.json".into()), keys: Some(KEY.into()), build: Some("7".into()) });
        assert_eq!(bundle_server(&file, None, true), Ok(Some("https://sync.example.com".into())), "both live in one file");
        assert_eq!(bundle_updater(&none, unset), Ok(Updater::default()), "a source build has no updater");
        let build_8 = |name: &str| (name == "CROSSBOW_BUILD_NUMBER").then(|| "8".to_owned());
        assert_eq!(bundle_updater(&file, build_8).unwrap().build.as_deref(), Some("8"), "the environment wins");
        let only_url = |name: &str| (name == "CROSSBOW_UPDATE_URL").then(|| "https://updates.example.com/stable.json".to_owned());
        assert_eq!(bundle_updater(&none, only_url).unwrap_err(), "an updater needs both CROSSBOW_UPDATE_URL and CROSSBOW_UPDATE_KEYS, or neither");
        let no_build = |name: &str| match name {
            "CROSSBOW_UPDATE_URL" => Some("https://updates.example.com/stable.json".to_owned()),
            "CROSSBOW_UPDATE_KEYS" => Some(KEY.to_owned()),
            _ => None,
        };
        assert!(bundle_updater(&none, no_build).unwrap_err().starts_with("a bundle with an updater needs CROSSBOW_BUILD_NUMBER"));
        let words = |name: &str| (name == "CROSSBOW_BUILD_NUMBER").then(|| "seven".to_owned());
        assert_eq!(bundle_updater(&none, words).unwrap_err(), "CROSSBOW_BUILD_NUMBER must be 1 to 18 digits");
        for dir in [file, none] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

fn bundle_output(target: &Path, args: &[String]) -> Result<PathBuf, String> {
    let Some(index) = args.iter().position(|arg| arg == "--candidate") else { return Ok(target.join("bundle")) };
    let name = args.get(index + 1).ok_or("--candidate needs a name")?;
    if name.is_empty() || name.len() > 64 || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
        return Err("candidate names use only letters, digits, hyphens and underscores".into());
    }
    let out = target.join(format!("candidate-{name}"));
    if out.exists() { return Err("candidate already exists; choose a new name to preserve its bytes".into()); }
    Ok(out)
}

#[cfg(target_os = "macos")]
fn platform_bundle(root: &Path, target: &Path, out: &Path, build: Option<&str>) -> Result<(), String> {
    // The executable inside the bundle is named after the app.
    let stage = target.join("bundle-stage");
    std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    std::fs::copy(target.join("crossbow"), stage.join("Crossbow")).map_err(|e| e.to_string())?;
    for helper in ["crossbow-helper", "crossbow-update"] {
        std::fs::copy(target.join(helper), stage.join(helper)).map_err(|e| e.to_string())?;
    }
    let app = assemble(root, &stage, out, build)?;
    // Ad-hoc signature over the whole bundle so macOS treats it as one app.
    run(Command::new("codesign").args(["--force", "--deep", "--sign", "-"]).arg(&app))?;
    println!("{}", app.display());
    Ok(())
}

/// What macOS shows when Crossbow first asks for a device. cef-rs fills these
/// in with the executable's name. A site still needs its own permission in
/// the profile; location isn't declared, so Crossbow refuses it.
#[cfg(target_os = "macos")]
const USAGE: [(&str, &str); 4] = [
    ("NSCameraUsageDescription", "Websites you allow can use your camera."),
    ("NSMicrophoneUsageDescription", "Websites you allow can use your microphone."),
    ("NSBluetoothAlwaysUsageDescription", "Websites can ask to use Bluetooth devices."),
    ("NSWebBrowserPublicKeyCredentialUsageDescription", "Websites can ask you to sign in with a passkey."),
];

/// Lays out Crossbow.app in `out` around the executables in `stage`, unsigned,
/// with `crossbow-update`, which finishes installing an update, beside the
/// app's executable, and `build` as its `CFBundleVersion`.
#[cfg(target_os = "macos")]
fn assemble(root: &Path, stage: &Path, out: &Path, build: Option<&str>) -> Result<PathBuf, String> {
    use cef::build_util::mac::{BundleInfo, bundle};
    let version = semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    let app = out.join("Crossbow.app");
    if app.exists() {
        std::fs::remove_dir_all(&app).map_err(|e| e.to_string())?;
    }
    let info = BundleInfo::new("Crossbow", "app.crossbow.Crossbow", "Crossbow", "en", version);
    let app = bundle(out, stage, "Crossbow", "crossbow-helper", Some(root.join("app/resources")), info).map_err(|e| format!("{e}"))?;

    // Embedded typefaces still require their licenses in the distributed bundle.
    copy_dir(&root.join("app/resources/fonts"), &app.join("Contents/Resources/fonts"))?;
    std::fs::copy(stage.join("crossbow-update"), app.join("Contents/MacOS/crossbow-update")).map_err(|e| e.to_string())?;
    // Helpers need their own identifiers; the main app registers as a browser.
    let fw = app.join("Contents/Frameworks");
    for (suffix, id) in [("Helper", "helper"), ("Helper (GPU)", "helper.gpu"), ("Helper (Renderer)", "helper.renderer"), ("Helper (Plugin)", "helper.plugin"), ("Helper (Alerts)", "helper.alerts")] {
        let plist = fw.join(format!("Crossbow {suffix}.app/Contents/Info.plist"));
        run(Command::new("plutil").args(["-replace", "CFBundleIdentifier", "-string", &format!("app.crossbow.Crossbow.{id}")]).arg(&plist))?;
    }
    let plist = app.join("Contents/Info.plist");
    let set = |key: &str, kind: &str, value: &str| run(Command::new("plutil").args(["-replace", key, kind, value]).arg(&plist));
    set("NSHighResolutionCapable", "-bool", "true")?;
    set("LSApplicationCategoryType", "-string", "public.app-category.productivity")?;
    set("NSHumanReadableCopyright", "-string", "Crossbow")?;
    if let Some(build) = build {
        set("CFBundleVersion", "-string", build)?;
    }
    set(
        "CFBundleURLTypes",
        "-json",
        r#"[{"CFBundleURLName":"Web site URL","CFBundleURLSchemes":["http","https"]},{"CFBundleURLName":"Crossbow","CFBundleURLSchemes":["crossbow"]}]"#,
    )?;
    set(
        "CFBundleDocumentTypes",
        "-json",
        r#"[{"CFBundleTypeName":"HTML document","CFBundleTypeRole":"Viewer","LSItemContentTypes":["public.html","public.xhtml"]}]"#,
    )?;
    for plist in plists(&app) {
        for (key, sentence) in USAGE {
            run(Command::new("plutil").args(["-replace", key, "-string", sentence]).arg(&plist))?;
        }
        // A newer cef-rs may add a key that still holds a name.
        let (executable, usage) = usage_strings(&plist)?;
        if let Some((key, _)) = usage.iter().find(|(_, value)| *value == executable) {
            return Err(format!("{key} in {} is only the executable's name", plist.display()));
        }
    }
    Ok(app)
}

/// The Info.plist of the app and of each helper.
#[cfg(target_os = "macos")]
fn plists(app: &Path) -> Vec<PathBuf> {
    let helpers = ["Helper", "Helper (GPU)", "Helper (Renderer)", "Helper (Plugin)", "Helper (Alerts)"];
    let fw = app.join("Contents/Frameworks");
    std::iter::once(app.join("Contents/Info.plist")).chain(helpers.map(|suffix| fw.join(format!("Crossbow {suffix}.app/Contents/Info.plist")))).collect()
}

/// The usage descriptions in `plist`, with its executable's name: the
/// `<key>`/`<string>` pairs of its XML form.
#[cfg(target_os = "macos")]
fn usage_strings(plist: &Path) -> Result<(String, Vec<(String, String)>), String> {
    let xml = Command::new("plutil").args(["-convert", "xml1", "-o", "-"]).arg(plist).output().map_err(|e| e.to_string())?;
    if !xml.status.success() { return Err(format!("plutil can't read {}", plist.display())); }
    let xml = String::from_utf8(xml.stdout).map_err(|e| e.to_string())?;
    let (mut executable, mut usage, mut rest) = (None, Vec::new(), xml.as_str());
    while let Some(start) = rest.find("<key>") {
        rest = &rest[start + 5..];
        let Some(end) = rest.find("</key>") else { break };
        let key = &rest[..end];
        rest = rest[end + 6..].trim_start();
        let Some(value) = rest.strip_prefix("<string>").and_then(|value| Some(&value[..value.find("</string>")?])) else { continue };
        if key == "CFBundleExecutable" {
            executable = Some(value.to_owned());
        } else if key.ends_with("UsageDescription") {
            usage.push((key.to_owned(), value.to_owned()));
        }
    }
    Ok((executable.ok_or_else(|| format!("{} names no executable", plist.display()))?, usage))
}

#[cfg(all(test, target_os = "macos"))]
mod bundle_tests {
    use super::*;

    /// Lays out a bundle around stand-in executables, under the test TMPDIR.
    fn assembled(name: &str) -> (PathBuf, Result<PathBuf, String>) {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        let stage = dir.join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("Crossbow"), b"stand-in").unwrap();
        for executable in ["crossbow-helper", "crossbow-update"] {
            std::fs::write(stage.join(executable), b"stand-in").unwrap();
        }
        let app = assemble(&root(), &stage, &dir.join("out"), Some("42"));
        (dir, app)
    }

    #[test]
    fn every_bundled_plist_explains_device_use_in_a_sentence() {
        let (dir, app) = assembled("usage");
        let checked = app.and_then(|app| plists(&app).iter().map(|plist| Ok((plist.clone(), usage_strings(plist)?))).collect::<Result<Vec<_>, String>>());
        std::fs::remove_dir_all(&dir).unwrap();
        let checked = checked.unwrap();
        assert_eq!(checked.len(), 6);
        for (plist, (executable, usage)) in checked {
            let value = |key: &str| usage.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
            assert_eq!(value("NSCameraUsageDescription"), Some("Websites you allow can use your camera."), "{}", plist.display());
            assert_eq!(value("NSMicrophoneUsageDescription"), Some("Websites you allow can use your microphone."), "{}", plist.display());
            for (key, value) in &usage {
                assert!(value != &executable && value.ends_with('.'), "{key} in {} is {value:?}", plist.display());
            }
            // Location stays refused until Crossbow declares it on purpose.
            assert!(value("NSLocationWhenInUseUsageDescription").is_none() && value("NSLocationUsageDescription").is_none());
        }
    }

    #[test]
    fn the_bundle_carries_the_update_helper_and_reads_as_an_update() {
        use crossbow_core::update::{MacVersion, Version, archive};
        let (dir, app) = assembled("update");
        let read = app.as_ref().map(|app| (std::fs::read(app.join("Contents/MacOS/crossbow-update")).ok(), archive::read_info(app)));
        std::fs::remove_dir_all(&dir).unwrap();
        let (helper, info) = read.unwrap();
        assert_eq!(helper.as_deref(), Some(&b"stand-in"[..]), "crossbow-update sits beside the app's executable");
        let bundle = info.expect("the updater reads the bundle's Info.plist");
        assert_eq!((bundle.version, bundle.build, bundle.executable.as_str()), (Version::parse(env!("CARGO_PKG_VERSION")).unwrap(), 42, "Crossbow"));
        assert!(bundle.minimum_macos >= MacVersion(11, 0, 0));
    }
}


#[cfg(target_os = "windows")]
fn platform_bundle(root: &Path, target: &Path, out: &Path, _build: Option<&str>) -> Result<(), String> {
    // CEF's runtime files sit next to the executable on Windows.
    let cef = PathBuf::from(std::env::var("CEF_PATH").map_err(|_| "set CEF_PATH to the CEF distribution")?);
    let out = out.join("Crossbow");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    copy_dir(&cef, &out)?;
    std::fs::copy(target.join("crossbow.exe"), out.join("Crossbow.exe")).map_err(|e| e.to_string())?;
    copy_dir(&root.join("app/resources/fonts"), &out.join("resources/fonts"))?;
    println!("{}", out.display());
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_bundle(root: &Path, target: &Path, out: &Path, _build: Option<&str>) -> Result<(), String> {
    let cef = PathBuf::from(std::env::var("CEF_PATH").map_err(|_| "set CEF_PATH to the CEF distribution")?);
    let out = out.join("crossbow");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    copy_dir(&cef, &out)?;
    std::fs::copy(target.join("crossbow"), out.join("crossbow")).map_err(|e| e.to_string())?;
    std::fs::copy(root.join("app/resources/icon-256.png"), out.join("crossbow.png")).map_err(|e| e.to_string())?;
    copy_dir(&root.join("app/resources/fonts"), &out.join("resources/fonts"))?;
    std::fs::write(
        out.join("crossbow.desktop"),
        "[Desktop Entry]\nType=Application\nName=Crossbow\nExec=crossbow %U\nIcon=crossbow\nCategories=Network;WebBrowser;\nMimeType=text/html;x-scheme-handler/http;x-scheme-handler/https;\n",
    )
    .map_err(|e| e.to_string())?;
    println!("{}", out.display());
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let to = dst.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
