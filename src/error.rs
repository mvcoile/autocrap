#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("USB error: {0}")]
    Usb(#[from] rusb::Error),

    #[error("MIDI init error")]
    MidiInit(#[from] midir::InitError),

    #[error("MIDI port info error: {0}")]
    MidiPortInfo(#[from] midir::PortInfoError),

    #[error("MIDI connect error: {0:?}")]
    MidiConnect(midir::ConnectErrorKind),

    #[error("MIDI send error: {0}")]
    MidiSend(#[from] midir::SendError),

    #[error("OSC error: {0}")]
    Osc(#[from] rosc::OscError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("channel receive error: {0}")]
    ChannelRecv(#[from] std::sync::mpsc::RecvError),

    #[error("channel send error")]
    ChannelSend,

    #[error("{0} endpoint not found")]
    EndpointNotFound(&'static str),
}
