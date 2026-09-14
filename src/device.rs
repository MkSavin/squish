use std::process::{Command, Stdio};

use crate::cli::Backend;

/// What the header says the work runs on, e.g. `gpu NVENC (NVIDIA GeForce RTX 4070 Ti)`.
pub fn describe(backend: Backend) -> String {
    let (engine, name) = match backend {
        Backend::Nvenc => ("gpu NVENC", nvidia_name()),
        Backend::VideoToolbox => ("gpu VideoToolbox", cpu_name()),
        Backend::Cpu => ("cpu", cpu_name()),
    };
    match name {
        Some(name) => format!("{engine} ({name})"),
        None => engine.to_owned(),
    }
}

fn nvidia_name() -> Option<String> {
    let output = run("nvidia-smi", &["--query-gpu=name", "--format=csv,noheader"])?;
    tidy(output.lines().next()?)
}

#[cfg(target_os = "windows")]
fn cpu_name() -> Option<String> {
    let output = run(
        "reg",
        &[
            "query",
            r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            "/v",
            "ProcessorNameString",
        ],
    )?;
    parse_reg_value(&output)
}

#[cfg(target_os = "linux")]
fn cpu_name() -> Option<String> {
    parse_cpuinfo(&std::fs::read_to_string("/proc/cpuinfo").ok()?)
}

#[cfg(target_os = "macos")]
fn cpu_name() -> Option<String> {
    tidy(&run("sysctl", &["-n", "machdep.cpu.brand_string"])?)
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn cpu_name() -> Option<String> {
    None
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `reg query` answers with a table: name, type, then the value itself.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_reg_value(output: &str) -> Option<String> {
    let line = output
        .lines()
        .find(|line| line.contains("REG_SZ") || line.contains("REG_EXPAND_SZ"))?;
    let (_, value) = line.split_once("REG_SZ")?;
    tidy(value)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_cpuinfo(cpuinfo: &str) -> Option<String> {
    let line = cpuinfo
        .lines()
        .find(|line| line.starts_with("model name") || line.starts_with("Model"))?;
    tidy(line.split_once(':')?.1)
}

fn tidy(name: &str) -> Option<String> {
    let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_answer_gives_up_its_value() {
        let output = "\r\nHKEY_LOCAL_MACHINE\\HARDWARE\\...\\0\r\n    ProcessorNameString    REG_SZ    13th Gen Intel(R) Core(TM) i7-13700K\r\n\r\n";
        assert_eq!(
            parse_reg_value(output).as_deref(),
            Some("13th Gen Intel(R) Core(TM) i7-13700K")
        );
        assert_eq!(parse_reg_value("nothing here"), None);
    }

    #[test]
    fn cpuinfo_gives_up_its_model_name() {
        let cpuinfo =
            "processor\t: 0\nmodel name\t: AMD Ryzen 9 7950X 16-Core Processor\nstepping\t: 2\n";
        assert_eq!(
            parse_cpuinfo(cpuinfo).as_deref(),
            Some("AMD Ryzen 9 7950X 16-Core Processor")
        );
        assert_eq!(parse_cpuinfo("processor\t: 0\n"), None);
    }

    #[test]
    fn names_lose_their_padding_and_double_spaces() {
        assert_eq!(
            tidy("  NVIDIA   GeForce RTX 4070 Ti \r\n").as_deref(),
            Some("NVIDIA GeForce RTX 4070 Ti")
        );
        assert_eq!(tidy("   "), None);
    }

    #[test]
    fn every_backend_names_itself_even_without_a_device() {
        for backend in [Backend::Nvenc, Backend::VideoToolbox, Backend::Cpu] {
            let text = describe(backend);
            assert!(
                text.starts_with("gpu ") || text.starts_with("cpu "),
                "{text}"
            );
        }
    }
}
