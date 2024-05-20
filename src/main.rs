// Benchmarking tool for OVMF

use std::process::{Command,Stdio};
use std::io::{BufRead, BufReader};
use std::os::unix::net::{UnixStream,UnixListener};
use std::{thread, time};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use plotters::prelude::*;
use plotters::style::colors;

const HV_PATH: &str = "/home/tobin/qemu/build/qemu-system-x86_64";
const KERNEL_PATH: &str = "/opt/kata/share/kata-containers/vmlinuz-confidential.container";
const INITRD_PATH: &str = "/home/tobin/kata-containers/tools/osbuilder/initrd-builder/kata-containers-initrd.img";
const FW_PATH: &str = "/home/tobin/edk2/Build/OvmfX64/DEBUG_GCC5/FV/OVMF.fd";

const DEBUG_SOCKET: &str = "/tmp/ovmf_output.sock";
fn main() {
    start_guest(false);
}

fn start_guest(sev_enabled: bool) {
    let mut cmd = Command::new("sudo");

    cmd.arg(HV_PATH);

    if sev_enabled {
        cmd.args(["-name","direct-snp"]);
        cmd.args(["-machine","q35,accel=kvm,kernel_irqchip=split,confidential-guest-support=sev0"]);
        cmd.args(["-object","sev-snp-guest,id=sev0,policy=0x30000,kernel-hashes=off,reduced-phys-bits=5,cbitpos=51"]);
    }
    else {
        cmd.args(["-name","direct-nosev"]);
        cmd.args(["-machine","q35,accel=kvm,kernel_irqchip=split"]);
    }

    // basic guest properties
    cmd.arg("-enable-kvm");
    cmd.args(["-cpu","EPYC-v4"]);
    cmd.args(["-smp","2"]);
    cmd.args(["-m","512"]);

    // artifacts
    cmd.args(["-initrd",INITRD_PATH]);
    cmd.args(["-kernel",KERNEL_PATH]);
    cmd.args(["-append","\"console=ttyS0\""]);

    let fw_drive = format!("if=pflash,format=raw,readonly=on,file={}",FW_PATH);
    cmd.args(["-drive",&fw_drive]);

    // devices
    cmd.arg("-nographic");
    cmd.args(["-device","virtio-scsi-pci,id=scsi,disable-legacy=on,iommu_platform=true"]);
    cmd.args(["-chardev","file,id=char0,path=serial-output.txt"]);
    cmd.args(["-serial","chardev:char0"]);

    let debug_dev = format!("socket,path={},id=fwdbg", DEBUG_SOCKET);
    cmd.args(["-chardev", &debug_dev]);
    cmd.args(["-device", "isa-debugcon,iobase=0x402,chardev=fwdbg"]);

    cmd.stdout(Stdio::null());

    let debug_log = Arc::new(Mutex::new(Vec::new()));

    let _ = std::fs::remove_file(DEBUG_SOCKET);
    let listener = UnixListener::bind(DEBUG_SOCKET).unwrap();

    println!("Starting Guest");
    let mut child = cmd.spawn().unwrap();

    let stream = listener.incoming().next().unwrap();
    let debug_log_clone = debug_log.clone();
    let handler = thread::spawn(move || handle_debug(stream.unwrap(), debug_log_clone));

	let ten_seconds = time::Duration::from_secs(10);
	thread::sleep(ten_seconds);

	let _ = child.kill();
    handler.join().unwrap();

    /*
    for line in debug_log.lock().unwrap().iter() {
        println!("{}",  line.0);
    }
    */

    // the log entries that define each phase
    let keypoints = vec![
        "SecCoreStartupWithStack",
        "Platform PEIM Loaded",
        "MpInitChangeApLoopCallback() done!",
    ];

    // get the times just for the keypoints
    let mut keypoint_times = HashMap::new();
    let mut previous_end_time = 0;

    for (message, timestamp) in debug_log.lock().unwrap().iter() {
        for keypoint in &keypoints {
            if message.contains(keypoint) {
                keypoint_times.insert(keypoint, (previous_end_time as i32, *timestamp as i32));
                previous_end_time = *timestamp;
				//print!("{} - {}\n", timestamp, keypoint);
                break;
            }
        }
    }
        
    // MAKE CHART
    let (graph_title, graph_filename) = match sev_enabled {
        true => ("OVMF Phases with SEV", "sev.png"),
        false => ("OVMF Phases without SEV", "nosev.png"),
    };

    let root = BitMapBackend::new(graph_filename, (900, 300)).into_drawing_area();
    root.fill(&WHITE).unwrap();

    let mut chart = ChartBuilder::on(&root)
        .caption(graph_title, ("sans-serif", 20).into_font())
        .margin(5)
        .x_label_area_size(30)
        .y_label_area_size(30)
        .build_cartesian_2d(0..5000, 0..2).unwrap();

    chart.configure_mesh().draw().unwrap();

    let height = 1;
    let colors = [colors::CYAN, colors::GREEN, colors::MAGENTA, colors::RED];

    for (i, (name, (start, end))) in keypoint_times.iter().enumerate() {
        //let (name, (start, end)) = keypoint;

        let color = colors[i];
        chart
            .draw_series(std::iter::once(Rectangle::new(
                [(*start, height), (*end, 0)],
                color.filled(),
            ))).unwrap()
            .label(&***name)
            .legend(move |(x, y)| Rectangle::new([(x, y - 5),(x + 10, y + 5)], color.filled()));
    }

    chart
        .configure_series_labels()
        .draw().unwrap();

    root.present().unwrap();

}

fn handle_debug(stream: UnixStream, debug_log: Arc<Mutex<Vec<(String, u128)>>>) {
	let stream = BufReader::new(stream);

    let now = time::Instant::now();

	for line in stream.lines() {
        let elapsed = now.elapsed().as_millis();
        debug_log.lock().unwrap().push((line.unwrap(), elapsed));
	}

}
