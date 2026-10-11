use std::error::Error;
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;

pub fn run(bin: &str, body: impl FnOnce() -> Result<(), Box<dyn Error>>) -> ExitCode {
    panic::set_hook(Box::new(|_| {}));
    let outcome = panic::catch_unwind(AssertUnwindSafe(body));
    let message = match outcome {
        Ok(Ok(())) => return ExitCode::SUCCESS,
        Ok(Err(e)) => e.to_string(),
        Err(payload) => {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown cause".into());
            format!("internal error: {what}")
        }
    };
    eprintln!("{bin}: {}", one_line(&message));
    ExitCode::FAILURE
}

fn one_line(text: &str) -> String {
    let parts: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if parts.is_empty() {
        "failed with no message".into()
    } else {
        parts.join("; ")
    }
}
