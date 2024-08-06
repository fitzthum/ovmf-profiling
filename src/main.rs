// Benchmarking tool for OVMF

use plotters::prelude::*;
use plotters::style::colors;
use std::io::{BufRead, BufReader};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::{thread, time};

const HV_PATH: &str = "/home/tobin/qemu/build/qemu-system-x86_64";
const KERNEL_PATH: &str = "/opt/kata/share/kata-containers/vmlinuz-confidential.container";
const INITRD_PATH: &str =
    "/home/tobin/kata-containers/tools/osbuilder/initrd-builder/kata-containers-initrd.img";
const FW_PATH: &str = "/home/tobin/edk2/Build/OvmfX64/DEBUG_GCC5/FV/OVMF.fd";

const DEBUG_SOCKET: &str = "/tmp/ovmf_output.sock";
const QMP_SOCKET: &str = "/tmp/ovmf_qmp.sock";
const SHARED_SOCKET: &str = "/tmp/ovmf_shared.sock";
const CHARDEV_SOCK: &str = "/tmp/chardev.sock";

const VERBOSE: bool = false;

pub enum GuestType {
    NoSev,
    Sev,
    SevEs,
    Snp,
}

trait ChartConfig {
    fn to_keypoints() -> Vec<String>;
}

struct BasicChart {}

impl ChartConfig for BasicChart {
    fn to_keypoints() -> Vec<String> {
        vec![
            "SecCoreStartupWithStack".to_string(), // the start of the log
            "Platform PEIM Loaded".to_string(),    // start of PEI?
            "Loading DXE CORE".to_string(),        // start of DXE
            //"EekDxeMain1".to_string(), // start of DXE
            //"[Variable]END_OF_DXE is signaled".to_string(), // end of DXE
            "EekDxeMain3".to_string(), // end of DXE
            //"EekBds1".to_string(), // start of BDS
            //"MpInitChangeApLoopCallback() done!".to_string(), // end of log
            "EekBds2".to_string(), // random place towards end of BDS
        ]
    }
}

trait ConfigFragment {
    fn to_command(guest_type: &GuestType) -> Command;
}

struct BasicGuest {}

impl ConfigFragment for BasicGuest {
    fn to_command(guest_type: &GuestType) -> Command {
        let mut cmd = Command::new("sudo");
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());

        cmd.arg(HV_PATH);

        match guest_type {
            GuestType::Snp => {
                cmd.args(["-name", "direct-snp"]);
                cmd.args([
                    "-machine",
                    "q35,accel=kvm,nvdimm=on,kernel_irqchip=split,confidential-guest-support=sev0",
                ]);
                cmd.args(["-object","sev-snp-guest,id=sev0,policy=0x30000,kernel-hashes=off,reduced-phys-bits=5,cbitpos=51"]);
            }
            GuestType::NoSev => {
                cmd.args(["-name", "direct-nosev"]);
                cmd.args(["-machine", "q35,accel=kvm,nvdimm=on,kernel_irqchip=split"]);
            }
            GuestType::Sev => {
                cmd.args(["-name", "direct-sev"]);
                cmd.args([
                    "-machine",
                    "q35,accel=kvm,nvdimm=on,kernel_irqchip=split,confidential-guest-support=sev0",
                ]);
                cmd.args([
                    "-object",
                    "sev-guest,id=sev0,cbitpos=51,reduced-phys-bits=1,policy=0x1",
                ]);
            }
            GuestType::SevEs => {
                cmd.args(["-name", "direct-seves"]);
                cmd.args([
                    "-machine",
                    "q35,accel=kvm,nvdimm=on,kernel_irqchip=split,confidential-guest-support=sev0",
                ]);
                cmd.args([
                    "-object",
                    "sev-guest,id=sev0,cbitpos=51,reduced-phys-bits=1,policy=0x5",
                ]);
            }
        }

        // basic guest properties
        cmd.arg("-enable-kvm");
        cmd.args(["-cpu", "EPYC-v4"]);
        cmd.args(["-smp", "2"]);
        cmd.args(["-m", "512M,slots=10,maxmem=257720M"]);
        //cmd.args(["-m","64G"]);

        // artifacts
        cmd.args(["-initrd", INITRD_PATH]);
        cmd.args(["-kernel", KERNEL_PATH]);
        cmd.args(["-append", "\"console=ttyS0\""]);

        let fw_drive = format!("if=pflash,format=raw,readonly=on,file={}", FW_PATH);
        cmd.args(["-drive", &fw_drive]);

        // devices
        cmd.arg("-nographic");

        cmd
    }
}

struct KataGuest {}

impl ConfigFragment for KataGuest {
    fn to_command(guest_type: &GuestType) -> Command {
        let mut cmd = BasicGuest::to_command(&guest_type);

        //cmd.args(["-device","virtio-scsi-pci,id=scsi,disable-legacy=on,iommu_platform=true"]);

        // from kata
        cmd.args(["-device", "virtio-scsi-pci,id=scsi,disable-modern=false"]);
        cmd.args(["-chardev", "file,id=char0,path=serial-output.txt"]);
        cmd.args(["-serial", "chardev:char0"]);

        let debug_dev = format!("socket,path={},id=fwdbg", DEBUG_SOCKET);
        cmd.args(["-chardev", &debug_dev]);
        cmd.args(["-device", "isa-debugcon,iobase=0x402,chardev=fwdbg"]);

        // add stuff used by kata
        cmd.args(["-device", "pci-bridge,bus=pcie.0,id=pci-bridge-0,chassis_nr=1,shpc=off,addr=4,io-reserve=4k,mem-reserve=1m,pref64-reserve=1m"]);

        cmd.args([
            "-device",
            "virtio-serial-pci,disable-modern=false,id=serial0",
        ]);
        cmd.args(["-object", "rng-random,id=rng0,filename=/dev/urandom"]);
        cmd.args(["-device", "virtio-rng-pci,rng=rng0"]);
        cmd.args(["-global", "kvm-pit.lost_tick_policy=discard"]);

        // slows down non-sev for some reason.
        // otherwise no effect
        let qmp = format!("unix:{},server=on,wait=off", QMP_SOCKET);
        cmd.args(["-qmp", &qmp]);

        // chardev for kata-shared
        // (requires some vhost setup?)

        //let shared_socket = format!("socket,id=char-shared,path={}", SHARED_SOCKET);
        //cmd.args(["-chardev", &shared_socket]);
        //cmd.args(["-device","vhost-user-fs-pci,chardev=char-shared,tag=kataShared,queue-size=1024"]);

        // not significant
        cmd.args(["-rtc", "base=utc,driftfix=slew,clock=host"]);

        // networking. no change
        cmd.args([
            "-netdev",
            "tap,id=network-0,script=qemu-ifup,downscript=no,ifname=\"tap0\",vhost=on",
        ]);
        cmd.args(["-device", "driver=virtio-net-pci,netdev=network-0,mac=ba:2f:08:16:18:aa,disable-modern=false,mq=on,vectors=4"]);

        // shared memory. no change
        cmd.args([
            "-object",
            "memory-backend-file,id=dimm1,size=512M,mem-path=/dev/shm,share=on",
        ]);
        cmd.args(["-numa", "node,memdev=dimm1"]);

        // virtconsole
        cmd.args(["-device", "virtconsole,chardev=charconsole0,id=console0"]);
        let chardev = format!(
            "socket,id=charconsole0,path={},server=on,wait=off",
            CHARDEV_SOCK
        );
        cmd.args(["-chardev", &chardev]);

        cmd
    }
}

fn main() {
    start_guest(GuestType::NoSev);
    start_guest(GuestType::Sev);
    start_guest(GuestType::SevEs);
    start_guest(GuestType::Snp);
}

fn start_guest(guest_type: GuestType) {
    // Generate QEMU Command
    let mut cmd = KataGuest::to_command(&guest_type);

    // Create Unix listener to capture output
    let _ = std::fs::remove_file(DEBUG_SOCKET);
    let listener = UnixListener::bind(DEBUG_SOCKET).unwrap();

    println!("Starting Guest");
    let mut child = cmd.spawn().unwrap();

    // Capture debug output
    let debug_log = Arc::new(Mutex::new(Vec::new()));
    let debug_log_clone = debug_log.clone();

    let stream = listener.incoming().next().unwrap();
    let handler = thread::spawn(move || handle_debug(stream.unwrap(), debug_log_clone));

    // Allow guest to run for 10 seconds
    let ten_seconds = time::Duration::from_secs(10);
    thread::sleep(ten_seconds);

    // Cleanup
    let _ = child.kill();
    handler.join().unwrap();

    if VERBOSE {
        for line in debug_log.lock().unwrap().iter() {
            println!("{} - {}", line.1, line.0);
        }
    }

    make_chart(debug_log, guest_type);
}

fn make_chart(debug_log: Arc<Mutex<Vec<(String, u128)>>>, guest_type: GuestType) {
    let keypoints = BasicChart::to_keypoints();

    // the log entries that define each phase
    // get the times just for the keypoints
    let mut keypoint_times = vec![];
    let mut previous_end_time = 0;

    for (message, time) in debug_log.lock().unwrap().iter() {
        for keypoint in &keypoints {
            if message.contains(keypoint) {
                let mut timestamp = *time;

                // fixup rollover
                if timestamp < previous_end_time {
                    timestamp += 16777215;
                }

                keypoint_times.push((keypoint, (previous_end_time as i32, timestamp as i32)));
                previous_end_time = timestamp;
                println!("{} - {}", timestamp, keypoint);
                break;
            }
        }
    }

    // MAKE CHART
    let (graph_title, graph_filename) = match guest_type {
        GuestType::Snp => ("OVMF Phases with SNP", "output/snp.png"),
        GuestType::NoSev => ("OVMF Phases without SEV", "output/nosev.png"),
        GuestType::Sev => ("OVMF Phases with SEV", "output/sev.png"),
        GuestType::SevEs => ("OVMF Phases with SEV-ES", "output/seves.png"),
    };

    let root = BitMapBackend::new(graph_filename, (900, 300)).into_drawing_area();
    root.fill(&WHITE).unwrap();

    let mut chart = ChartBuilder::on(&root)
        .caption(graph_title, ("sans-serif", 20).into_font())
        .margin(5)
        .x_label_area_size(30)
        .y_label_area_size(30)
        //.build_cartesian_2d(0..7000, 0..2).unwrap();
        //.build_cartesian_2d(0..2500000, 0..2).unwrap();
        .build_cartesian_2d(0..20000000, 0..2)
        .unwrap();

    chart.configure_mesh().draw().unwrap();

    let height = 1;
    let colors = [
        colors::CYAN,
        colors::GREEN,
        colors::MAGENTA,
        colors::RED,
        colors::YELLOW,
        colors::BLACK,
    ];

    for (i, (name, (start, end))) in keypoint_times.iter().enumerate() {
        //let (name, (start, end)) = keypoint;

        let color = colors[i % colors.len()];
        chart
            .draw_series(std::iter::once(Rectangle::new(
                [(*start, height), (*end, 0)],
                color.filled(),
            )))
            .unwrap()
            .label(&***name)
            .legend(move |(x, y)| Rectangle::new([(x, y - 5), (x + 10, y + 5)], color.filled()));
    }

    chart.configure_series_labels().draw().unwrap();

    root.present().unwrap();
}

fn handle_debug(stream: UnixStream, debug_log: Arc<Mutex<Vec<(String, u128)>>>) {
    let stream = BufReader::new(stream);

    //let now = time::Instant::now();

    for line in stream.lines() {
        //let elapsed = now.elapsed().as_millis();

        if let Ok(l) = line {
            let parts = l.split(" TICKS=").collect::<Vec<_>>();
            debug_log
                .lock()
                .unwrap()
                .push((parts[0].to_string(), parts[1].parse().unwrap()));
        }
    }
}
