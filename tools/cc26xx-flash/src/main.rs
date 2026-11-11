//! cc26xx-flash: back up and flash a Sonoff Zigbee 3.0 USB Dongle Plus
//! (TI CC2652P) through its ROM serial bootloader. See `--help`.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cc26xx_flash::ccfg::{self, Ccfg};
use cc26xx_flash::crc::crc32;
use cc26xx_flash::ops::{self, CC2652P_FLASH, ChipInfo};
use cc26xx_flash::proto::Bsl;
use cc26xx_flash::serial::SerialLink;

const USAGE: &str = "\
cc26xx-flash: back up and flash a Sonoff Zigbee 3.0 USB Dongle Plus (CC2652P)
through the chip's ROM serial bootloader.

USAGE:
    cc26xx-flash --port PATH [OPTIONS] info
    cc26xx-flash --port PATH [OPTIONS] read OUT.bin [--len BYTES]
    cc26xx-flash --port PATH [OPTIONS] flash IMAGE.hex|IMAGE.bin [--yes]

COMMANDS:
    info    Enter the bootloader, print the chip ID, flash size and the
            CCFG's bootloader settings, then reset. Changes nothing.
    read    Back up flash to OUT.bin (default: all of it), checked against
            the chip's own CRC32 of the same range. Refuses to overwrite.
    flash   Write an image: check that its CCFG keeps the bootloader
            reachable, bank erase, write, verify by CRC32, reset. To go
            back to Zigbee, flash the backup made with `read`.

OPTIONS:
    --port PATH     The dongle's serial port. Use its /dev/serial/by-id/
                    path, which names this exact dongle.
    --baud N        Bootloader baud rate (default 115200; it autobauds).
    --len BYTES     For read: how much to read, e.g. 0x58000.
    --dry-run       Do not open the port: check the image (flash) and print
                    what would be done.
    --yes           For flash: do not ask for confirmation.
    -h, --help      This text.
";

struct Opts {
    port: Option<String>,
    baud: u32,
    dry_run: bool,
    yes: bool,
    len: Option<u32>,
    cmd: Vec<String>,
}

fn parse_num(s: &str) -> Option<u32> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        port: None,
        baud: 115_200,
        dry_run: false,
        yes: false,
        len: None,
        cmd: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |n: &str| args.next().ok_or_else(|| format!("{n} needs a value"));
        match a.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--port" => o.port = Some(value("--port")?),
            "--baud" => o.baud = parse_num(&value("--baud")?).ok_or("--baud: not a number")?,
            "--len" => o.len = Some(parse_num(&value("--len")?).ok_or("--len: not a number")?),
            "--dry-run" => o.dry_run = true,
            "--yes" => o.yes = true,
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => o.cmd.push(a),
        }
    }
    Ok(o)
}

type Link = Bsl<SerialLink>;

fn open(o: &Opts) -> Result<Link, String> {
    let port = o.port.as_deref().ok_or("--port is required")?;
    if !port.starts_with("/dev/serial/by-id/") {
        eprintln!(
            "warning: {port} is not a /dev/serial/by-id/ path; ttyUSB numbers change when \
             devices are plugged in, so make sure this is the dongle"
        );
    }
    let link = SerialLink::open(port, o.baud).map_err(|e| format!("{port}: {e}"))?;
    let mut bsl = Bsl::new(link);
    ops::connect(&mut bsl).map_err(|e| {
        format!(
            "could not reach the ROM bootloader: {e}\n(Is something else using the port? \
             Is this a Sonoff Zigbee 3.0 USB Dongle Plus with the CP2102N bridge?)"
        )
    })?;
    Ok(bsl)
}

fn identify(bsl: &mut Link) -> Result<ChipInfo, String> {
    let info = ops::identify(bsl).map_err(|e| e.to_string())?;
    eprintln!("bootloader: ping OK");
    eprintln!("{}", info.summary());
    eprintln!("chip CCFG: {}", ccfg::describe(&info.ccfg));
    Ok(info)
}

fn finish(bsl: &mut Link) {
    match bsl.reset() {
        Ok(()) => eprintln!("reset: the chip is starting its firmware"),
        Err(e) => {
            eprintln!("RESET command failed ({e}); pulsing reset with RTS instead");
            let _ = bsl.reset_by_pins_sonoff();
        }
    }
}

fn progress_printer() -> impl FnMut(&str, usize, usize) {
    let mut last = (String::new(), usize::MAX);
    move |what: &str, done: usize, total: usize| {
        let pct = (done * 100).checked_div(total).unwrap_or(100);
        let step = pct / 10;
        if what != last.0 || step != last.1 {
            eprint!("\r{what}: {pct:3}%  ");
            if pct == 100 {
                eprintln!();
            }
            let _ = std::io::stderr().flush();
            last = (what.to_string(), step);
        }
    }
}

fn cmd_info(o: &Opts) -> Result<(), String> {
    if o.dry_run {
        println!(
            "dry run: would enter the bootloader on {:?}, read the chip ID, flash size and CCFG, then reset",
            o.port
        );
        return Ok(());
    }
    let mut bsl = open(o)?;
    let info = identify(&mut bsl)?;
    let r = ccfg::check(&info.ccfg, info.sectors);
    if r.safe() {
        println!("the firmware now on the chip keeps the bootloader reachable through DIO15");
    } else {
        println!(
            "the firmware now on the chip has CCFG problems:\n  - {}",
            r.fatal.join("\n  - ")
        );
    }
    for n in &r.notes {
        println!("note: {n}");
    }
    finish(&mut bsl);
    Ok(())
}

fn cmd_read(o: &Opts, out: &Path) -> Result<(), String> {
    if out.exists() {
        return Err(format!(
            "{} exists; not overwriting a backup",
            out.display()
        ));
    }
    if o.dry_run {
        println!(
            "dry run: would enter the bootloader, read 0x{:X} bytes of flash into {}, compare with the chip's CRC32, then reset",
            o.len.unwrap_or(CC2652P_FLASH),
            out.display()
        );
        return Ok(());
    }
    let mut bsl = open(o)?;
    let info = identify(&mut bsl)?;
    let len = o.len.unwrap_or(info.flash_size());
    if !len.is_multiple_of(4) || len > info.flash_size() {
        return Err(format!(
            "--len 0x{len:X} must be a multiple of 4 and at most 0x{:X}",
            info.flash_size()
        ));
    }
    let mut p = progress_printer();
    let (data, crc) = ops::backup(&mut bsl, len, &mut |d, t| p("read", d as usize, t as usize))
        .map_err(|e| e.to_string())?;
    let tmp = out.with_extension("partial");
    std::fs::write(&tmp, &data).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, out).map_err(|e| format!("{}: {e}", out.display()))?;
    println!(
        "read 0x{len:X} bytes to {}; CRC32 0x{crc:08X} matches the chip's",
        out.display()
    );
    if let Some(c) = Ccfg::from_image(&data).filter(|_| len == info.flash_size()) {
        if c.image_valid != 0 {
            println!(
                "this backup holds no bootable image (IMAGE_VALID_CONF is not 0): an erased or half-written chip"
            );
        }
        let r = ccfg::check(&c, info.sectors);
        println!(
            "backup CCFG {} keep the bootloader reachable, so it can be flashed back with this tool",
            if r.safe() { "would" } else { "would NOT" }
        );
    }
    finish(&mut bsl);
    Ok(())
}

fn confirm(image: &Path) -> Result<(), String> {
    if !std::io::stdin().is_terminal() {
        return Err("not a terminal, so cannot ask; pass --yes to flash without asking".into());
    }
    eprint!(
        "This erases the dongle and writes {}. Type yes to continue: ",
        image.display()
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.trim() == "yes" {
        Ok(())
    } else {
        Err("not confirmed; nothing was changed".into())
    }
}

fn cmd_flash(o: &Opts, image_path: &Path) -> Result<(), String> {
    let image = ops::load_image(image_path, CC2652P_FLASH)?;
    let report = ops::preflight(&image, CC2652P_FLASH)?;
    let c = Ccfg::from_image(&image).unwrap();
    let runs = ops::runs(&image);
    let bytes: usize = runs.iter().map(|(s, e)| e - s).sum();
    println!(
        "image {}: 0x{:X} bytes, CRC32 0x{:08X}",
        image_path.display(),
        image.len(),
        crc32(&image)
    );
    println!("image CCFG: {}", ccfg::describe(&c));
    println!("CCFG check: OK, the bootloader stays reachable through DIO15 (active low)");
    for n in &report.notes {
        println!("note: {n}");
    }
    println!(
        "to write: {bytes} bytes in {} run(s) (blank 0xFF chunks are skipped)",
        runs.len()
    );
    if o.dry_run {
        println!(
            "dry run: would enter the bootloader, bank erase, write, verify by CRC32 and reset"
        );
        return Ok(());
    }
    if !o.yes {
        confirm(image_path)?;
    }
    let mut bsl = open(o)?;
    let info = identify(&mut bsl)?;
    if info.flash_size() != CC2652P_FLASH {
        return Err(format!(
            "this chip has 0x{:X} bytes of flash; the image is for 0x{CC2652P_FLASH:X}",
            info.flash_size()
        ));
    }
    let mut p = progress_printer();
    let done = ops::program_with_retry(
        &mut bsl,
        &image,
        3,
        &mut |w, d, t| p(w, d, t),
        &mut |n, e| {
            eprintln!("\nattempt {n} failed: {e}\nre-entering the bootloader and starting again")
        },
    )
    .map_err(|e| {
        format!(
            "flashing failed: {e}\nThe chip is erased or partly written and will stay in its ROM \
             bootloader; run the same flash command again (or flash your backup)."
        )
    })?;
    println!(
        "erase: {}; wrote {} bytes in {} packets; verify: chip CRC32 0x{:08X} matches the image",
        match done.erase {
            ops::Erase::Bank => "bank erase",
            ops::Erase::Sectors => "sector by sector (bank erase refused)",
        },
        done.bytes_sent,
        done.packets,
        done.crc
    );
    finish(&mut bsl);
    Ok(())
}

fn main() -> ExitCode {
    let o = match parse_args() {
        Ok(o) => o,
        Err(e) if e.is_empty() => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let r = match o.cmd.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["info"] => cmd_info(&o),
        ["read", out] => cmd_read(&o, &PathBuf::from(out)),
        ["flash", img] => cmd_flash(&o, &PathBuf::from(img)),
        _ => Err(format!("expected a command\n\n{USAGE}")),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
