//! Terminal questions for `luxid new`.
//!
//! Hand-rolled rather than pulling in a prompt crate. The questions are two
//! short multiple-choice lists, and rolling them means full control over the
//! part that actually matters: what happens when there is no terminal.
//!
//! A scaffolding tool that blocks on stdin inside a CI job, a Docker build or
//! a shell pipeline is a scaffolding tool people stop trusting. Every prompt
//! here is reachable only when [`is_interactive`] is true; otherwise the caller
//! takes the default.

use std::io::{self, BufRead, IsTerminal, Write};

/// Whether there is a human at both ends.
///
/// Both streams are checked: stdout redirected to a file means the question
/// would never be seen, even though stdin could still be answered.
pub fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Ask a multiple-choice question, returning the index of the chosen option.
///
/// `options` is `(label, description)`. `default` is the index used when the
/// answer is empty, which is what pressing Enter does.
///
/// Numbered rather than arrow-key driven: it works over ssh, in a dumb
/// terminal, and inside editors that do not forward key events — and it can be
/// answered by piping a number in, which arrow keys cannot.
pub fn choose(question: &str, options: &[(&str, &str)], default: usize) -> usize {
    let width = options
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0);

    loop {
        println!();
        println!("{question}");
        println!();

        for (index, (label, description)) in options.iter().enumerate() {
            let marker = if index == default { " (default)" } else { "" };

            println!("  {}) {label:width$}  {description}{marker}", index + 1);
        }

        println!();
        print!("  [1-{}] ", options.len());
        let _ = io::stdout().flush();

        let mut answer = String::new();

        // A closed stdin cannot be recovered from by asking again, which would
        // spin forever. Take the default and move on.
        if io::stdin().lock().read_line(&mut answer).unwrap_or(0) == 0 {
            println!();
            return default;
        }

        let answer = answer.trim();

        if answer.is_empty() {
            return default;
        }

        match answer.parse::<usize>() {
            Ok(choice) if choice >= 1 && choice <= options.len() => return choice - 1,
            _ => println!("\n  `{answer}` is not one of the options."),
        }
    }
}
