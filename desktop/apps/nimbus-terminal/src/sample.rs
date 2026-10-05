// SPDX-License-Identifier: MIT

//! A deterministic terminal session for screenshots: prompts, colored `ls` and `git` output,
//! a system monitor line, box drawing, and text in several scripts.

use std::fmt::Write as _;

const RESET: &str = "\x1b[0m";

fn prompt(out: &mut String, dir: &str, branch: &str) {
    // Powerline-style segments.
    let _ = write!(
        out,
        "\x1b[48;5;25;38;5;255;1m ada@nimbus \x1b[0;38;5;25;48;5;238m\u{E0B0}\x1b[38;5;253m {dir} \
         \x1b[38;5;238;48;5;29m\u{E0B0}\x1b[38;5;255m \u{2387} {branch} {RESET}\x1b[38;5;29m\u{E0B0}{RESET} "
    );
}

fn command(out: &mut String, dir: &str, text: &str) {
    prompt(out, dir, "main");
    let _ = write!(out, "{text}\r\n");
}

fn bar(out: &mut String, label: &str, fill: f32, width: usize, text: &str) {
    let filled = ((width as f32) * fill).round() as usize;
    let green = filled * 6 / 10;
    let yellow = filled * 3 / 10;
    let red = filled - green - yellow;
    let _ = write!(out, "\x1b[36m{label}\x1b[0;1m[{RESET}");
    let _ = write!(
        out,
        "\x1b[32m{}\x1b[33m{}\x1b[31m{}",
        "|".repeat(green),
        "|".repeat(yellow),
        "|".repeat(red)
    );
    let pad = width.saturating_sub(filled + text.len());
    let _ = write!(out, "{}\x1b[90m{text}\x1b[0;1m]{RESET}", " ".repeat(pad));
}

/// The bytes of the sample session.
pub fn session() -> Vec<u8> {
    let mut out = String::new();
    let home = "~/projects/nimbus";

    command(&mut out, home, "ls --color=auto");
    let _ = write!(
        out,
        "\x1b[1;34mapps\x1b[0m   Cargo.lock  \x1b[1;34mcrates\x1b[0m  \x1b[1;32mdeploy.sh\x1b[0m  \x1b[1;34mdocs\x1b[0m  \
         \x1b[1;35mlogo.png\x1b[0m  \x1b[1;31mnimbus-0.1.tar.gz\x1b[0m  README.md  \x1b[1;36mtarget\x1b[0m\r\n"
    );

    command(&mut out, home, "git log --oneline --graph -4");
    for (graph, hash, refs, message) in [
        (
            "*",
            "3f9c2ab",
            " (\x1b[1;36mHEAD -> \x1b[1;32mmain\x1b[33m, \x1b[1;31morigin/main\x1b[33m)",
            "terminal: render damaged rows only",
        ),
        ("*", "b81e07d", "", "shell: add the notification center"),
        ("*", "52d1c9e", " (\x1b[1;33mtag: v0.1.0\x1b[33m)", "compositor: tiling layout with gaps"),
        ("*", "0a7f3e1", "", "theme: oklch accent states"),
    ] {
        let _ = write!(out, "\x1b[31m{graph}\x1b[0m \x1b[33m{hash}{refs}\x1b[0m {message}\r\n");
    }

    command(&mut out, home, "cargo test -p nimbus-terminal --release");
    let _ = write!(
        out,
        "\x1b[1;32m    Finished\x1b[0m `release` profile [optimized] target(s) in 41.07s\r\n\
         \x1b[1;32m     Running\x1b[0m unittests src/lib.rs\r\n\
         test result: \x1b[32mok\x1b[0m. 84 passed; 0 failed; 0 ignored; finished in 1.92s\r\n"
    );

    command(&mut out, home, "htop");
    bar(&mut out, "  1", 0.62, 30, "62.4%");
    out.push_str("   ");
    bar(&mut out, "  2", 0.28, 30, "28.1%");
    out.push_str(
        "   \x1b[1mTasks: \x1b[36m214\x1b[0m, \x1b[32m612 thr\x1b[0m; \x1b[32m3\x1b[0m running\r\n",
    );
    bar(&mut out, "Mem", 0.45, 30, "7.1G/15.6G");
    out.push_str("   ");
    bar(&mut out, "Swp", 0.04, 30, "0K/8.0G");
    out.push_str("   \x1b[1mLoad average: \x1b[0;1m1.24 \x1b[0m0.98 \x1b[90m0.77\x1b[0m\r\n");
    let _ = write!(
        out,
        "\x1b[30;42m    PID USER       PRI  NI  VIRT   RES  CPU% MEM%   TIME+  Command                         {RESET}\r\n\
         \x1b[30;46m   4211 ada         20   0 2.1G  412M  38.2  2.6  4:12.08 nimbus-compositor --backend udev {RESET}\r\n\
         \x20  4388 ada         20   0  812M 96.4M  \x1b[1m12.7\x1b[0m  0.6  0:41.55 \x1b[1mnimbus-terminal\x1b[0m\r\n\
         \x20   917 \x1b[90mroot\x1b[0m        20   0  1.4G  41.2M   1.3  0.3  0:07.91 \x1b[90m/usr/lib/systemd/systemd-logind\x1b[0m\r\n"
    );

    command(&mut out, home, "cat docs/hello.txt");
    let _ = write!(
        out,
        "\x1b[38;5;245m┌──────────────┬────────────────────────────────┐\x1b[0m\r\n\
         \x1b[38;5;245m│\x1b[0m \x1b[1mLanguage\x1b[0m     \x1b[38;5;245m│\x1b[0m \x1b[1mGreeting\x1b[0m                       \x1b[38;5;245m│\x1b[0m\r\n\
         \x1b[38;5;245m├──────────────┼────────────────────────────────┤\x1b[0m\r\n\
         \x1b[38;5;245m│\x1b[0m 中文         \x1b[38;5;245m│\x1b[0m 你好，世界                     \x1b[38;5;245m│\x1b[0m\r\n\
         \x1b[38;5;245m│\x1b[0m 日本語       \x1b[38;5;245m│\x1b[0m こんにちは                     \x1b[38;5;245m│\x1b[0m\r\n\
         \x1b[38;5;245m│\x1b[0m Ελληνικά     \x1b[38;5;245m│\x1b[0m Γειά σου κόσμε · ∑ π ≈ 3.14159 \x1b[38;5;245m│\x1b[0m\r\n\
         \x1b[38;5;245m╰──────────────┴────────────────────────────────╯\x1b[0m\r\n"
    );
    let _ = write!(
        out,
        "\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m \x1b[4:3;58;5;196mundercurl\x1b[0m \
         \x1b[9mstrike\x1b[0m \x1b[2mdim\x1b[0m \x1b[7m inverse \x1b[0m  "
    );
    for i in 0..16 {
        let _ = write!(out, "\x1b[48;5;{i}m  ");
    }
    out.push_str(RESET);
    out.push_str("  ");
    for i in 0..16 {
        let t = i as f32 / 15.0;
        let (r, g, b) =
            ((53.0 + 180.0 * t) as u8, (132.0 - 60.0 * t) as u8, (228.0 - 120.0 * t) as u8);
        let _ = write!(out, "\x1b[48;2;{r};{g};{b}m ");
    }
    out.push_str(RESET);
    out.push_str("\r\n");

    prompt(&mut out, home, "main");
    out.push_str("cargo run --release -p nimbus-compositor");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{feed, test_term};
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line};

    fn row_text<T>(term: &alacritty_terminal::Term<T>, line: i32) -> String {
        let row = &term.grid()[Line(line)];
        (0..term.columns()).map(|c| row[Column(c)].c).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn session_renders_into_a_terminal() {
        let mut term = test_term(110, 34);
        feed(&mut term, &session());
        let text: Vec<String> = (0..34).map(|l| row_text(&term, l)).collect();
        assert!(
            text[0].contains("ada@nimbus") && text[0].ends_with("ls --color=auto"),
            "{:?}",
            text[0]
        );
        assert!(text.iter().any(|l| l.contains("test result: ok. 84 passed")));
        assert!(text.iter().any(|l| l.contains("你 好")), "wide characters take two cells");
        assert!(text.iter().any(|l| l.starts_with('┌')));
        assert!(text.iter().any(|l| l.ends_with("nimbus-compositor")), "{text:#?}");
        // Every line fits without wrapping.
        let wrapped = (0..34).filter(|l| {
            term.grid()[Line(*l)][Column(109)]
                .flags
                .contains(alacritty_terminal::term::cell::Flags::WRAPLINE)
        });
        assert_eq!(wrapped.count(), 0);
    }
}
