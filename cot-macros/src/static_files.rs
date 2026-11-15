use std::path::Path;

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{LitStr, Token};

use crate::cot_ident;

pub(super) fn static_files_to_tokens(input: proc_macro::TokenStream) -> TokenStream {
    let paths = match Punctuated::<LitStr, Token![,]>::parse_terminated.parse(input) {
        Ok(paths) => paths,
        Err(error) => return error.to_compile_error(),
    };

    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo");
    let static_dir = Path::new(&manifest_dir).join("static");

    let mut files = Vec::new();
    for path in &paths {
        match collect_files(&static_dir, &path.value()) {
            Ok(mut found) => files.append(&mut found),
            Err(message) => return syn::Error::new(path.span(), message).to_compile_error(),
        }
    }

    let cot = cot_ident();
    let files = files.iter().map(|StaticFileSource { path, full_path }| {
        quote! {
            #cot::static_files::StaticFile::new(
                #path,
                #cot::__private::Bytes::from_static(::std::include_bytes!(#full_path)),
            )
        }
    });

    quote! {
        ::std::vec![#(#files),*]
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct StaticFileSource {
    /// The path relative to the `static` directory, used as the file's key.
    path: String,
    /// The absolute path to the file, passed to `include_bytes!`.
    full_path: String,
}

/// Returns the file at `path` under `static_dir`, or every file in the
/// directory at `path`, recursively and sorted by path.
fn collect_files(static_dir: &Path, path: &str) -> Result<Vec<StaticFileSource>, String> {
    let full_path = static_dir.join(path);
    let metadata = std::fs::metadata(&full_path).map_err(|_| {
        format!("could not find static file or directory `{path}` in the `static` directory")
    })?;

    if metadata.is_dir() {
        let mut files = Vec::new();
        collect_dir(&full_path, path.trim_end_matches('/'), &mut files)?;
        files.sort();
        Ok(files)
    } else {
        Ok(vec![StaticFileSource {
            path: path.to_owned(),
            full_path: path_to_string(&full_path)?,
        }])
    }
}

fn collect_dir(
    dir: &Path,
    dir_path: &str,
    files: &mut Vec<StaticFileSource>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("could not read static directory `{dir_path}`: {error}"))?;

    for entry in entries {
        let entry = entry
            .map_err(|error| format!("could not read static directory `{dir_path}`: {error}"))?;
        let name = entry.file_name().into_string().map_err(|name| {
            format!(
                "static file names must be valid UTF-8, but `{dir_path}` contains {}",
                name.display()
            )
        })?;
        let path = if dir_path.is_empty() {
            name
        } else {
            format!("{dir_path}/{name}")
        };
        let full_path = entry.path();

        // `metadata` follows symlinks, so linked files and directories are
        // included like regular ones
        let metadata = std::fs::metadata(&full_path)
            .map_err(|error| format!("could not read static file `{path}`: {error}"))?;
        if metadata.is_dir() {
            collect_dir(&full_path, &path, files)?;
        } else {
            files.push(StaticFileSource {
                full_path: path_to_string(&full_path)?,
                path,
            });
        }
    }

    Ok(())
}

fn path_to_string(path: &Path) -> Result<String, String> {
    path.to_str().map(ToOwned::to_owned).ok_or_else(|| {
        format!(
            "static file paths must be valid UTF-8, but got {}",
            path.display()
        )
    })
}
