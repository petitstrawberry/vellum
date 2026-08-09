//! Vellum command-line entry point.

use std::path::PathBuf;

use vellum_core::Document;

#[cfg(feature = "gui")]
mod ui;

struct Arguments {
    path: Option<PathBuf>,
    page: usize,
    dump_png: Option<PathBuf>,
}

fn main() {
    let arguments = match parse_arguments() {
        Ok(arguments) => arguments,
        Err(error) => {
            eprintln!("vellum: {error}");
            print_usage();
            return;
        }
    };

    if let Some(output) = arguments.dump_png {
        let Some(path) = arguments.path else {
            eprintln!("vellum: --dump-png requires an input document");
            return;
        };
        if let Err(error) = dump_page(&path, arguments.page, &output) {
            eprintln!("vellum: {error}");
        }
        return;
    }

    #[cfg(feature = "gui")]
    {
        let (document, status) = match arguments.path {
            Some(path) => match Document::open(&path) {
                Ok(document) => {
                    let status = format!(
                        "{} — {} page{}",
                        document.title(),
                        document.page_count(),
                        if document.page_count() == 1 { "" } else { "s" }
                    );
                    (Some(document), status)
                }
                Err(error) => (None, format!("Could not open document: {error}")),
            },
            None => (
                None,
                String::from("Pass an image or PDF path to open it in Vellum"),
            ),
        };

        if let Err(error) = ui::run(document, arguments.page, status) {
            eprintln!("vellum: {error}");
        }
    }

    #[cfg(not(feature = "gui"))]
    {
        eprintln!("vellum: this build has no GUI; use --dump-png to rasterize a page");
        print_usage();
    }
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut path = None;
    let mut page = 0usize;
    let mut dump_png = None;
    let mut arguments = std::env::args().skip(1);

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            "--page" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| String::from("--page requires a zero-based page number"))?;
                page = value
                    .parse()
                    .map_err(|_| String::from("--page must be a zero-based page number"))?;
            }
            "--dump-png" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| String::from("--dump-png requires an output path"))?;
                dump_png = Some(PathBuf::from(value));
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown option: {value}"));
            }
            value => {
                if path.is_some() {
                    return Err(String::from(
                        "only one input document can be opened at a time",
                    ));
                }
                path = Some(PathBuf::from(value));
            }
        }
    }

    Ok(Arguments {
        path,
        page,
        dump_png,
    })
}

fn dump_page(path: &PathBuf, page: usize, output: &PathBuf) -> Result<(), String> {
    let document = Document::open(path).map_err(|error| error.to_string())?;
    let bitmap = document.page(page).map_err(|error| error.to_string())?;
    bitmap.save_png(output).map_err(|error| error.to_string())
}

fn print_usage() {
    eprintln!("usage: vellum [--page N] <image-or-file.pdf>");
    eprintln!("       vellum --dump-png <output.png> [--page N] <image-or-file.pdf>");
}
