use std::path::Path;

use desktop_core::ipc::{MESSAGE_CHANNEL_BUFFER, NATIVE_MESSAGING_BUFFER_SIZE};
use futures::{SinkExt, StreamExt};
use log::*;
use tokio_util::codec::LengthDelimitedCodec;

fn init_logging(log_path: &Path, level: log::LevelFilter) {
    use simplelog::{ColorChoice, CombinedLogger, Config, SharedLogger, TermLogger, TerminalMode};

    let config = Config::default();

    let mut loggers: Vec<Box<dyn SharedLogger>> = Vec::new();
    loggers.push(TermLogger::new(
        level,
        config.clone(),
        TerminalMode::Stderr,
        ColorChoice::Auto,
    ));

    match std::fs::File::create(log_path) {
        Ok(file) => {
            loggers.push(simplelog::WriteLogger::new(level, config, file));
        }
        Err(e) => {
            eprintln!("Can't create file: {}", e);
        }
    }

    if let Err(e) = CombinedLogger::init(loggers) {
        eprintln!("Failed to initialize logger: {}", e);
    }
}

/// Bitwarden IPC Proxy.
///
/// This proxy allows browser extensions to communicate with a desktop application using Native
/// Messaging. This method allows an extension to send and receive messages through the use of
/// stdin/stdout streams.
///
/// However, this also requires the browser to start the process in order for the communication to
/// occur. To overcome this limitation, we implement Inter-Process Communication (IPC) to establish
/// a stable communication channel between the proxy and the running desktop application.
///
/// Browser extension <-[native messaging]-> proxy <-[ipc]-> desktop
///
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let sock_path = desktop_core::ipc::path("bitwarden");

    let log_path = {
        let mut path = sock_path.clone();
        path.set_extension("bitwarden.log");
        path
    };

    init_logging(&log_path, LevelFilter::Info);

    info!("Starting Bitwarden IPC Proxy.");

    // Different browsers send different arguments when the app starts:
    //
    // Firefox:
    // - The complete path to the app manifest. (in the form `/Users/<user>/Library/.../Mozilla/NativeMessagingHosts/com.8bit.bitwarden.json`)
    // - (in Firefox 55+) the ID (as given in the manifest.json) of the add-on that started it (in the form `{[UUID]}`).
    //
    // Chrome on Windows:
    // - Origin of the extension that started it (in the form `chrome-extension://[ID]`).
    // - Handle to the Chrome native window that started the app.
    //
    // Chrome on Linux and Mac:
    // - Origin of the extension that started it (in the form `chrome-extension://[ID]`).

    let args: Vec<_> = std::env::args().skip(1).collect();
    info!("Process args: {:?}", args);

    // Setup two channels, one for sending messages to the desktop application (`out`) and one for receiving messages from the desktop application (`in`)
    let (in_send, in_recv) = tokio::sync::mpsc::channel(MESSAGE_CHANNEL_BUFFER);
    let (out_send, mut out_recv) = tokio::sync::mpsc::channel(MESSAGE_CHANNEL_BUFFER);

    let mut handle = tokio::spawn(desktop_core::ipc::client::connect(
        sock_path, out_send, in_recv,
    ));

    // Create a new codec for reading and writing messages from stdin/stdout.
    let mut stdin = LengthDelimitedCodec::builder()
        .max_frame_length(NATIVE_MESSAGING_BUFFER_SIZE)
        .native_endian()
        .new_read(tokio::io::stdin());
    let mut stdout = LengthDelimitedCodec::builder()
        .max_frame_length(NATIVE_MESSAGING_BUFFER_SIZE)
        .native_endian()
        .new_write(tokio::io::stdout());

    loop {
        tokio::select! {
            // IPC client has finished, so we should exit as well.
            _ = &mut handle => {
                break;
            }

            // Receive messages from IPC and print to STDOUT.
            msg = out_recv.recv() => {
                match msg {
                    Some(msg) => {
                        debug!("OUT: {}", msg);
                        stdout.send(msg.into()).await.unwrap();
                    }
                    None => {
                        info!("Channel closed, exiting.");
                        break;
                    }
                }
            },

            // Listen to stdin and send messages to ipc processor.
            msg = stdin.next() => {
                match msg {
                    Some(Ok(msg)) => {
                        let m = String::from_utf8(msg.to_vec()).unwrap();
                        debug!("IN: {}", m);
                        in_send.send(m).await.unwrap();
                    }
                    Some(Err(e)) => {
                        error!("Error parsing input: {}", e);
                        break;
                    }
                    None => {
                        info!("Received EOF, exiting.");
                        break;
                    }
                }
            }

        }
    }
}
