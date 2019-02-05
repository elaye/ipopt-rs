//   Copyright 2018 Egor Larionov
//
//   Licensed under the Apache License, Version 2.0 (the "License");
//   you may not use this file except in compliance with the License.
//   You may obtain a copy of the License at
//
//       http://www.apache.org/licenses/LICENSE-2.0
//
//   Unless required by applicable law or agreed to in writing, software
//   distributed under the License is distributed on an "AS IS" BASIS,
//   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//   See the License for the specific language governing permissions and
//   limitations under the License.

use curl::easy::Easy;
use flate2::read::GzDecoder;
use lazy_static::lazy_static;
/**
 * # Goals
 *
 * 1. Make this library build without external dependencies on all platforms. This allows us to run
 *    this library through a third party CI easily.
 * 2. Use all available local dependencies to build this library if any to make the build process
 *    as fast as possible. This also allows developers with optimized/personalized blas
 *    environments to use their configuration without needing to customize this build process.
 *
 *
 * This build file is responsible for:
 *
 * 1. Finding or building Ipopt.
 * 2. Building the custom ipopt_cnlp interface linking to the found/buld Ipopt in step 1.
 * 3. Building this Rust library.
 *
 *
 * # Build Process
 *
 * 1. To satisfy goal 2. first use pkg-config to find any preinstalled ipopt libs.
 * 2. If nothing is found in pkg-config, we try to build ipopt from source on supoorted platforms
 *    with MKL (currently only macOS) to maintain the most optimal build.
 * 3. To satisfy goal 1. if the steps above fail, we will download the prebuild libraries from
 *    https://github.com/JuliaOpt/IpoptBuilder/releases which is already referenced on the official
 *    Ipopt webpage as a source of prebuilt binaries.
 */
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};
use tar::Archive;

const LIBRARY: &str = "ipopt";
const SOURCE_URL: &str = "https://www.coin-or.org/download/source/Ipopt";
const VERSION: &str = "3.12.10";
const MIN_VERSION: &str = "3.12.8";
const BINARY_DL_URL: &str = "https://github.com/JuliaOpt/IpoptBuilder/releases/download/";

#[cfg(target_os = "macos")]
mod platform {
    pub static LIB_EXT: &str = "dylib";
    pub static BINARY_SUFFIX: &str = "x86_64-apple-darwin14.tar.gz";
    pub static BINARY_MD5: &str = "59825a6b7e40929ff2c88fb23dc82b7c";
    pub static BINARY_SHA1: &str = "a24f1def1ce9fc33393779b574cea9bfb4765c4f";
}

#[cfg(target_os = "linux")]
mod platform {
    pub static LIB_EXT: &str = "so";
    pub static BINARY_SUFFIX: &str = "x86_64-linux-gnu-gcc8.tar.gz";
    pub static BINARY_MD5: &str = "9c406cb1b54918b56945548e64b8e9ca";
    pub static BINARY_SHA1: &str = "a940b1f70021ddbd057643a056b61228d68f26e6";
}

#[cfg(target_family = "unix")]
mod family {
    pub static LIB_MAJ_VER: &str = "1";
    pub static LIB_MIN_VER: &str = "10.10";
}

#[cfg(target_os = "windows")]
mod platform {
    pub static LIB_EXT: &str = "dll";
    pub static BINARY_SUFFIX: &str = "x86_64-w64-mingw32-gcc8.tar.gz";
}

#[cfg(target_os = "windows")]
mod family {
}

use crate::platform::*;
use crate::family::*;

lazy_static! {
    static ref BINARY_NAME: String = format!(
        "IpoptBuilder.v{ver}.{suffix}",
        ver = VERSION,
        suffix = BINARY_SUFFIX
    );
    static ref BINARY_URL: String = format!(
        "{dl}v{ver}-1-static/{name}",
        dl = BINARY_DL_URL,
        ver = VERSION,
        name = BINARY_NAME.as_str()
    );
}

fn main() {
    // Try to find Ipopt preinstalled.
    if let Ok(lib) = pkg_config::Config::new()
        .atleast_version(MIN_VERSION)
        .probe(LIBRARY)
    {
        dbg!(lib);
        unimplemented!();
    }

    let mut msg = String::from("\n\n");

    match build_and_install_ipopt() {
        Ok(ipopt_install_path) => {
            link(build_cnlp(ipopt_install_path)).expect("Failed to create bindings for Ipopt library.");;
            return;
        }
        Err(err) => {
            msg.push_str(&format!("Failed to build Ipopt from source: {:?}\n\n", err));
        }
    }

    match download_and_install_prebuilt_binary() {
        Ok(ipopt_install_path) => {
            link(build_cnlp(ipopt_install_path)).expect("Failed to create bindings for Ipopt library.");
            return;
        }
        Err(err) => {
            msg.push_str(&format!("Failed to download and install Ipopt binaries: {:?}\n\n", err));
        }
    }

    panic!(msg);
}

#[derive(Clone, Debug, PartialEq)]
enum Error {
    MKLInstallNotFound,
    DownloadFailure { response_code: u32, url: String },
    UrlFailure,
    UnsupportedPlatform,
    IOError,
    HashMismatch,
}

impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Error {
        Error::IOError
    }
}

impl From<curl::Error> for Error {
    fn from(_: curl::Error) -> Error {
        Error::UrlFailure
    }
}

fn library_name() -> String {
    format!("lib{}.{}", LIBRARY, LIB_EXT)
}

#[cfg(target_family = "windows")]
fn versioned_library_name() -> String {
    // No versioning in filenames on Windows.
    format!("lib{}.{}", LIBRARY, LIB_EXT)
}

#[cfg(target_family = "unix")]
fn versioned_library_name() -> String {
    if cfg!(target_os = "macos") {
        format!("lib{}.{}.{}.{}", LIBRARY, LIB_MAJ_VER, LIB_MIN_VER, LIB_EXT)
    } else {
        format!("lib{}.{}.{}.{}", LIBRARY, LIB_EXT, LIB_MAJ_VER, LIB_MIN_VER)
    }
}

#[cfg(target_family = "unix")]
fn major_versioned_library_name() -> String {
    if cfg!(target_os = "macos") {
        format!("lib{}.{}.{}", LIBRARY, LIB_MAJ_VER, LIB_EXT)
    } else {
        format!("lib{}.{}.{}", LIBRARY, LIB_EXT, LIB_MAJ_VER)
    }
}

/// Download the ipopt prebuilt binary from JuliaOpt and install it.
fn download_and_install_prebuilt_binary() -> Result<PathBuf, Error> {
    let file_name = BINARY_NAME.clone();

    // Extract the filename from the URL
    let mut base_name = file_name.clone();
    remove_suffix(&mut base_name, ".tar.gz");
    dbg!(&base_name);

    // Create download directory if it doesn't yet exist
    let crate_dir = PathBuf::from(&env::var("CARGO_MANIFEST_DIR").unwrap());
    let target_dir = crate_dir.join("target");
    dbg!(&target_dir);
    if !target_dir.exists() {
        fs::create_dir(&target_dir).unwrap();
    }

    let download_dir = target_dir.join(format!("ipopt-{}-binaries", VERSION));
    dbg!(&download_dir);
    if !download_dir.exists() {
        fs::create_dir(&download_dir).unwrap();
    }

    // Download and extract the tarball if the library isn't there.
    let unpacked_dir = download_dir.join(base_name);
    let output = PathBuf::from(&env::var("OUT_DIR").unwrap());
    let install_dir = output.clone();
    let library_file = versioned_library_name();
    let lib_dir = install_dir.join("lib");
    let library_path = lib_dir.join(&library_file);
    if library_path.exists() {
        // Nothing to be done, library is already installed
        return Ok(install_dir);
    }

    // On unix make sure all artifacts are removed to cleanup the environment
    if cfg!(target_family = "unix") {
        fs::remove_file(lib_dir.join(major_versioned_library_name())).ok();
        fs::remove_file(lib_dir.join(library_name())).ok();
    }

    // Build destination path
    let tarball_path = download_dir.join(file_name);
    dbg!(&tarball_path);

    if !unpacked_dir.exists() {
        download_tarball(&tarball_path, &BINARY_URL)?;
        check_tarball_hashes(&tarball_path)?;
        extract_tarball(tarball_path, &unpacked_dir);
    }

    // Copy lib
    if !lib_dir.exists() {
        fs::create_dir(&lib_dir)?;
    }
    fs::copy(
        unpacked_dir.join("lib").join(&library_file),
        &library_path,
    )
    .unwrap();

    // Make links (on unix only)
    if cfg!(target_family = "unix") {
        use std::os::unix::fs::symlink;
        symlink(&library_path, lib_dir.join(major_versioned_library_name()))?;
        symlink(&library_path, lib_dir.join(library_name()))?;
    }

    // Copy headers
    let install_include_dir = install_dir.join("include").join("coin");
    if !install_include_dir.exists() {
        fs::create_dir_all(&install_include_dir)?;
    }

    let include_dir = unpacked_dir.join("include").join("coin");
    for entry in fs::read_dir(include_dir)? {
        let file = entry?;
        fs::copy(file.path(), install_include_dir.join(file.file_name())).unwrap();
    }

    Ok(install_dir)
}

fn check_tarball_hashes(tarball_path: &Path) -> Result<(), Error> {
    use std::io::Read;
    use crypto::digest::Digest;

    {
        let mut f = File::open(tarball_path)?;
        let mut buffer = Vec::new();
        f.read_to_end(&mut buffer)?;
        let mut hasher = crypto::md5::Md5::new();
        hasher.input(&buffer);
        let dl_hex = hasher.result_str();
        if BINARY_MD5 != dl_hex {
            return Err(Error::HashMismatch);
        }
    }
    {
        let mut f = File::open(tarball_path)?;
        let mut buffer = Vec::new();
        f.read_to_end(&mut buffer)?;
        let mut hasher = crypto::sha1::Sha1::new();
        hasher.input(&buffer);
        let dl_hex = hasher.result_str();
        if BINARY_SHA1 != dl_hex {
            return Err(Error::HashMismatch);
        }
    }

    Ok(())
}

fn build_cnlp(ipopt_install_dir: PathBuf) -> PathBuf {
    cmake::Config::new("cnlp")
        .define("Ipopt_DIR:STRING", ipopt_install_dir.to_str().unwrap())
        .build()
}

fn link(cnlp_install_path: PathBuf) -> Result<(), Error> {
    // Link to cnlp
    println!(
        "cargo:rustc-link-search=native={}",
        cnlp_install_path.join("lib").display()
    );
    println!("cargo:rustc-link-lib=dylib=ipopt_cnlp");

    // Generate raw bindings to CNLP interface
    let c_api_header = cnlp_install_path.join("include").join("c_api.h");

    let bindings = bindgen::builder()
        .header(c_api_header.to_str().unwrap())
        .generate()
        .expect("Unable to generate bindings!");

    let output = PathBuf::from(&env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(output.join("ipopt_cnlp.rs"))
        .expect("Couldn't write bindings!");

    Ok(())
}

/// Download a tarball if it doesn't already exist.
fn download_tarball(tarball_path: &Path, binary_url: &str) -> Result<(), Error> {
    if !tarball_path.exists() {
        let f = File::create(tarball_path).unwrap();
        let mut writer = BufWriter::new(f);
        let mut easy = Easy::new();
        easy.follow_location(true)?;
        easy.url(binary_url).unwrap();
        easy.write_function(move |data| Ok(writer.write(data).unwrap()))
            .unwrap();
        easy.perform().unwrap();

        let response_code = easy.response_code().unwrap();
        if response_code != 200 {
            return Err(Error::DownloadFailure {
                response_code,
                url: binary_url.to_string(),
            });
        }
    }

    Ok(())
}

/// Build Ipopt statically linked to MKL if possible. Return the path to the ipopt library.
fn build_and_install_ipopt() -> Result<PathBuf, Error> {
    // Compile ipopt from source
    // Build URL to download from
    let binary_url = format!("{}/Ipopt-{}.tgz", SOURCE_URL, VERSION);
    dbg!(&binary_url);

    // Extract the filename from the URL
    let file_name = binary_url.split("/").last().unwrap();
    let mut base_name = file_name.to_string();
    remove_suffix(&mut base_name, ".tgz");
    dbg!(&base_name);

    // Create download directory if it doesn't yet exist
    let crate_dir = PathBuf::from(&env::var("CARGO_MANIFEST_DIR").unwrap());
    let target_dir = crate_dir.join("target");
    dbg!(&target_dir);
    if !target_dir.exists() {
        fs::create_dir(&target_dir).unwrap();
    }

    let download_dir = target_dir.join(format!("ipopt-{}-source", VERSION));
    dbg!(&download_dir);
    if !download_dir.exists() {
        fs::create_dir(&download_dir).unwrap();
    }

    // Download, extract and compile the tarball if the library isn't there.
    let unpacked_dir = download_dir.join(base_name);
    let output = PathBuf::from(&env::var("OUT_DIR").unwrap());
    let install_dir = output.clone();
    let library_file = format!("lib{}.{}", LIBRARY, LIB_EXT);
    let library_path = install_dir.join("lib").join(&library_file);
    if library_path.exists() {
        // Nothing to be done, library is already installed.
        return Ok(install_dir);
    }

    // Build destination path
    let tarball_path = download_dir.join(file_name);
    dbg!(&tarball_path);

    download_tarball(&tarball_path, &binary_url)?;
    extract_tarball(tarball_path, &download_dir);

    // Configure and compile
    // We shall compile ipopt in the same mode we build the sys library. This will allow users
    // to debug the internals of ipopt more easily.
    let debug: bool = env::var("DEBUG").unwrap().parse().unwrap();
    dbg!(debug);

    let build_dir = unpacked_dir
        .join("build")
        .join(if debug { "debug" } else { "release" });

    if !build_dir.exists() {
        fs::create_dir_all(&build_dir).unwrap();
    }

    // Remember project root directory
    let proj_root_dir = env::current_dir().unwrap();
    env::set_current_dir(build_dir).unwrap();

    let res = build_ipopt(&install_dir, debug);

    // Restore current directory
    env::set_current_dir(proj_root_dir).unwrap();

    res?; // Propagate any errors after we have restored the current dir.

    Ok(install_dir)
}

// Build Ipopt with MKL in the current directory.
fn build_ipopt(install_dir: &Path, debug: bool) -> Result<(), Error> {
    // Look for intel MKL and link to its libraries if found.
    let mkl_root = PathBuf::from(env::var("MKLROOT").unwrap_or("/opt/intel/mkl".to_string()));
    dbg!(&mkl_root);

    let blas = {
        if !mkl_root.exists() {
            return Err(Error::MKLInstallNotFound);
        } else {
            let mkl_prefix = format!("{}/lib/libmkl_", mkl_root.display());
            let link_libs = format!(
                "-L{mkl}/lib -ltbb -lpthread -lm -ldl",
                mkl = mkl_root.display()
            );
            if cfg!(target_os = "macos") {
                format!(
                    "--with-blas={mkl}intel_lp64.a {mkl}tbb_thread.a {mkl}core.a -lc++ {}",
                    link_libs,
                    mkl = mkl_prefix
                )
            } else {
                // Currently only support building Ipopt with MKL on macOS.
                return Err(Error::UnsupportedPlatform);
            }
        }
    };

    run(env::current_dir()?.parent().unwrap().parent().unwrap()
        .join("configure").to_str().unwrap(), |cmd| {
        let cmd = cmd
            .arg(format!("--prefix={}", install_dir.display()))
            .arg("--enable-shared")
            .arg("--disable-static")
            .arg(blas.clone());

        if debug {
            cmd.arg(format!("--enable-debug-ipopt"))
        } else {
            cmd
        }
    });

    run("make", |cmd| cmd.arg(format!("-j{}", num_cpus::get())));
    //run("make", |cmd| cmd.arg("test")); // Ensure everything is working
    run("make", |cmd| cmd.arg("install")); // Install to install_dir
    Ok(())
}

fn remove_suffix(value: &mut String, suffix: &str) {
    if value.ends_with(suffix) {
        let n = value.len();
        value.truncate(n - suffix.len());
    }
}

fn extract_tarball<P: AsRef<Path> + std::fmt::Debug, P2: AsRef<Path> + std::fmt::Debug>(archive_path: P, extract_to: P2) {
    println!("Extracting tarball {:?} to {:?}", &archive_path, &extract_to);
    let file = File::open(archive_path).unwrap();
    let mut a = Archive::new(GzDecoder::new(file));
    a.unpack(extract_to).unwrap();
}

fn run<F>(name: &str, mut configure: F)
where
    F: FnMut(&mut Command) -> &mut Command,
{
    let mut command = Command::new(name);
    let configured = configure(&mut command);
    println!("Executing {:?}", configured);
    if !configured.status().unwrap().success() {
        panic!("failed to execute {:?}", configured);
    }
    println!("Command {:?} finished successfully", configured);
}
