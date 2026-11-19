// Copyright © 2016-2026 The SENTIENT Authors

//! The Docker engine's offline path (crate::offline): the Ubuntu image,
//! Docker Engine and the stack's images from the pack, never from the
//! network. `docker compose pull` and `docker pull` are not run offline;
//! the images are `docker load`ed from the pack and compose starts them
//! with `--pull never`.

use std::path::PathBuf;

use super::{CH_IMAGE, DeployConfig, PG_IMAGE_STOCK, PG_IMAGE_TIMESCALE};
use crate::pack::{Kind, Opened};
use crate::progress::{Progress, ProgressFn};

/// The images `cfg`'s stack runs: its PostgreSQL, its ClickHouse (when it
/// has one) and SENTIENT.
pub fn stack_images(cfg: &DeployConfig) -> Vec<String> {
    let pg = if cfg.flavor.uses_timescaledb() || cfg.keep_timescale_image { PG_IMAGE_TIMESCALE } else { PG_IMAGE_STOCK };
    let mut v = vec![pg.to_string()];
    if cfg.flavor.uses_clickhouse() {
        v.push(CH_IMAGE.to_string());
    }
    v.push(cfg.image.clone());
    v
}

/// The ClickHouse-only stack's image (Windows hybrid).
pub fn clickhouse_images() -> Vec<String> {
    vec![CH_IMAGE.to_string()]
}

/// How the images of a step are had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageStep {
    /// Online: `docker compose pull`.
    Pull,
    /// Offline: `docker load -i` each (image, file in the pack).
    Load(Vec<(String, PathBuf)>),
}

/// The images `images` from `pack` (None: online, they are pulled). An
/// image the pack does not carry is refused by name; `present` says which
/// are on this machine already (those need no file).
pub fn image_step(images: &[String], pack: Option<&Opened>, present: &dyn Fn(&str) -> bool) -> Result<ImageStep, String> {
    let Some(pack) = pack else { return Ok(ImageStep::Pull) };
    let mut load = Vec::new();
    for image in images {
        if present(image) {
            continue;
        }
        match pack.image(image) {
            Some(f) => load.push((image.clone(), pack.path_of(f))),
            None => {
                return Err(format!(
                    "The offline pack does not carry the image {image}, and it is not on this computer. Use the pack of this SENTIENT release (it carries every image), or connect to the internet."
                ))
            }
        }
    }
    Ok(ImageStep::Load(load))
}

/// `docker load` of the file at `path` (as the shell that runs Docker
/// sees it). Docker reads gzip and zstd `docker save` files itself.
pub fn load_cmd(path: &str) -> String {
    format!("docker load -i {}", super::shell_quote(path))
}

/// A Windows path as the WSL distro sees it (Linux: itself).
pub fn shell_path(path: &std::path::Path) -> String {
    if cfg!(windows) { crate::clickhouse_wsl::wsl_path(path) } else { path.to_string_lossy().into_owned() }
}

/// Load `images` from the pack (those not on this machine yet), then check
/// that each is there under its name.
pub fn load_images(sink: &ProgressFn, pack: &Opened, images: &[String]) -> Result<(), String> {
    let present = |image: &str| super::run_output(&format!("docker image inspect {} >/dev/null 2>&1", super::shell_quote(image))).is_some_and(|(ok, _)| ok);
    let ImageStep::Load(files) = image_step(images, Some(pack), &present)? else { return Ok(()) };
    sink(Progress::Log { line: "Offline pack: the images are loaded from the pack (nothing is pulled)".into() });
    for (i, (image, file)) in files.iter().enumerate() {
        if crate::cancel::is_cancelled() {
            return Err("Cancelled.".into());
        }
        sink(Progress::Log { line: format!("Loading {image} from the offline pack ({} of {})", i + 1, files.len()) });
        sink(Progress::Percent { value: i as f32 / files.len().max(1) as f32 });
        super::run_stream(sink, &load_cmd(&shell_path(file))).map_err(|e| format!("The image {image} could not be loaded from the offline pack ({e})."))?;
        if !present(image) {
            return Err(format!("The offline pack's {} loaded, but not as {image}. The pack does not match this release.", file.display()));
        }
    }
    Ok(())
}

/// The Ubuntu root file system for `wsl --import`: the pack's.
pub fn rootfs(pack: &Opened) -> Result<PathBuf, String> {
    let f = pack.file(Kind::Rootfs).ok_or("The offline pack has no Ubuntu image for WSL (a rootfs file), so Docker in WSL 2 cannot be set up from it.")?;
    Ok(pack.path_of(f))
}

/// Install Docker Engine in the distro from the pack's packages: one
/// `dpkg -i` with all of them (dpkg orders their configuration by their
/// dependencies), in the pack's order. A Docker already there is kept.
pub fn docker_install_cmd(debs: &[String]) -> String {
    let list: Vec<String> = debs.iter().map(|d| super::shell_quote(d)).collect();
    format!(
        "[ -x /usr/bin/docker ] || {{ export DEBIAN_FRONTEND=noninteractive; dpkg -i {} || {{ echo 'The Docker packages of the offline pack did not install: a package they need is missing from the pack.' >&2; exit 1; }}; }}",
        list.join(" ")
    )
}

/// [`docker_install_cmd`] for `pack`'s packages.
pub fn docker_install(pack: &Opened) -> Result<String, String> {
    let debs = pack.files(Kind::Deb);
    if debs.is_empty() {
        return Err("The offline pack has no Docker Engine packages (.deb files), so Docker in WSL 2 cannot be set up from it.".into());
    }
    Ok(docker_install_cmd(&debs.iter().map(|f| shell_path(&pack.path_of(f))).collect::<Vec<_>>()))
}

/// `compose up -d`, which in offline mode never pulls.
pub fn up_cmd(cfg: &DeployConfig, offline: bool) -> String {
    cfg.compose_cmd(if offline { "up -d --pull never" } else { "up -d" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stack::Flavor;

    fn pack() -> Opened {
        crate::pack::debug_fake()
    }

    #[test]
    fn offline_the_images_are_loaded_from_the_pack_never_pulled() {
        let cfg = DeployConfig { flavor: Flavor::Timescaledb, image: "quakestring/sentient:4.4.0.4".into(), ..DeployConfig::default() };
        let images = stack_images(&cfg);
        assert_eq!(images, ["timescale/timescaledb:2.26.1-pg18", "quakestring/sentient:4.4.0.4"]);
        // Online: pulled.
        assert_eq!(image_step(&images, None, &|_| false), Ok(ImageStep::Pull));
        // Offline: each from its file, none pulled.
        let p = pack();
        match image_step(&images, Some(&p), &|_| false).unwrap() {
            ImageStep::Load(files) => {
                assert_eq!(files.len(), 2);
                assert!(files[0].1.ends_with("images/timescaledb-2.26.1-pg18.tar.zst") || files[0].1.to_string_lossy().ends_with("timescaledb-2.26.1-pg18.tar.zst"));
                assert_eq!(files[1].0, "quakestring/sentient:4.4.0.4");
            }
            other => panic!("{other:?}"),
        }
        // What is already on the machine is not loaded again.
        assert_eq!(image_step(&images, Some(&p), &|_| true), Ok(ImageStep::Load(Vec::new())));
        // The ClickHouse account's stack: ClickHouse alone.
        match image_step(&clickhouse_images(), Some(&p), &|_| false).unwrap() {
            ImageStep::Load(files) => assert_eq!(files[0].0, "clickhouse/clickhouse-server:24.8-alpine"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_image_the_pack_lacks_is_named() {
        let cfg = DeployConfig { image: "quakestring/sentient:4.4.0.9".into(), ..DeployConfig::default() };
        let e = image_step(&stack_images(&cfg), Some(&pack()), &|_| false).unwrap_err();
        assert!(e.contains("does not carry the image quakestring/sentient:4.4.0.9"), "{e}");
    }

    #[test]
    fn offline_compose_never_pulls_and_docker_comes_from_the_debs() {
        let cfg = DeployConfig::default();
        assert!(up_cmd(&cfg, true).ends_with("up -d --pull never"));
        assert!(up_cmd(&cfg, false).ends_with(" up -d"));
        assert_eq!(load_cmd("/mnt/e/pack/images/a b.tar.zst"), "docker load -i '/mnt/e/pack/images/a b.tar.zst'");
        let cmd = docker_install(&pack()).unwrap();
        assert!(cmd.starts_with("[ -x /usr/bin/docker ] || {"), "{cmd}");
        assert!(cmd.contains("dpkg -i ") && cmd.contains("package-0.deb") && cmd.contains("package-26.deb"));
        assert!(!cmd.contains("curl") && !cmd.contains("get.docker.com") && !cmd.contains("apt-get"));
        // A pack without them says so.
        let mut bare = pack();
        bare.pack.files.retain(|f| f.kind != Kind::Deb && f.kind != Kind::Rootfs);
        assert!(docker_install(&bare).unwrap_err().contains("no Docker Engine packages"));
        assert!(rootfs(&bare).unwrap_err().contains("no Ubuntu image"));
        assert!(rootfs(&pack()).unwrap().to_string_lossy().contains("rootfs.tar.gz"));
    }
}
