use log::{info, warn};
use rosc::{OscMessage, OscType};

use super::schema::{Config, CtrlKind, Mapping, MidiKind, MidiSpec, OnOffMode, RelativeMode};

type CtrlLogicBox = Box<dyn CtrlLogic>;
type CtrlConstructor = Box<dyn Fn(&Mapping) -> Option<CtrlLogicBox>>;

#[derive(Debug)]
pub struct Interpreter {
    ctrls: Vec<CtrlLogicBox>,
}

impl Interpreter {
    pub fn new(config: &Config) -> Interpreter {
        let constructors: Vec<CtrlConstructor> = vec![
            Box::new(OnOffLogic::from_mapping),
            Box::new(EightBitLogic::from_mapping),
            Box::new(RelativeLogic::from_mapping),
        ];
        let mut ctrls: Vec<CtrlLogicBox> = vec![];
        for abstract_mapping in config.mappings.iter() {
            for mapping in abstract_mapping.expand_iter() {
                let mut logic_opt: Option<CtrlLogicBox> = None;

                for make_logic in &constructors {
                    let Some(logic) = make_logic(&mapping) else {
                        continue;
                    };

                    logic_opt = Some(logic);
                    break;
                }

                let Some(logic) = logic_opt else {
                    warn!("unhandled mapping {:?}", mapping);
                    continue;
                };

                info!("adding {:?}", logic);
                ctrls.push(logic);
            }
        }

        Interpreter { ctrls }
    }

    pub fn handle_ctrl(&mut self, num: u8, val: u8) -> Option<Response> {
        for ctrl in &mut self.ctrls {
            let Some(response) = ctrl.handle_ctrl(num, val) else {
                continue;
            };

            return Some(response);
        }

        None
    }

    pub fn handle_osc(&mut self, msg: &OscMessage) -> Option<Response> {
        for ctrl in &mut self.ctrls {
            let Some(response) = ctrl.handle_osc(msg) else {
                continue;
            };

            return Some(response);
        }

        None
    }

    pub fn handle_midi(&mut self, msg: &[u8]) -> Option<Response> {
        for ctrl in &mut self.ctrls {
            let Some(response) = ctrl.handle_midi(msg) else {
                continue;
            };

            return Some(response);
        }

        None
    }
}

pub trait CtrlLogic: core::fmt::Debug + Send + Sync {
    fn from_mapping(mapping: &Mapping) -> Option<Box<dyn CtrlLogic>>
    where
        Self: Sized;
    fn handle_ctrl(&mut self, num: u8, val: u8) -> Option<Response>;
    fn handle_osc(&mut self, msg: &OscMessage) -> Option<Response>;
    fn handle_midi(&mut self, msg: &[u8]) -> Option<Response>;
}

#[derive(Debug)]
pub struct OnOffLogic {
    mode: OnOffMode,
    ctrl_in_num: Option<u8>,
    ctrl_out_num: Option<u8>,
    midi: Option<MidiSpec>,
    osc_addr: String,
    state: bool,
}

impl OnOffLogic {
    fn update(&mut self, new_state: bool, remember: bool) -> Response {
        if remember {
            let changed = new_state != self.state;
            self.state = new_state;

            if !changed {
                return Response::new();
            }
        }

        Response {
            osc: Some(OscResponse {
                addr: self.osc_addr.clone(),
                args: vec![OscType::Float(if new_state { 1.0 } else { 0.0 })],
            }),
            ctrl: self.ctrl_out_num.map(|num| CtrlResponse {
                data: vec![num, if new_state { 0x7f } else { 0x00 }],
            }),
            midi: self.midi.map(|midi| {
                let data = match midi.kind {
                    MidiKind::Cc => {
                        vec![
                            0b10110000 | midi.channel,
                            midi.num,
                            if new_state { 0x7f } else { 0x00 },
                        ]
                    }
                };
                MidiResponse { data }
            }),
        }
    }
}

impl CtrlLogic for OnOffLogic {
    fn from_mapping(mapping: &Mapping) -> Option<Box<dyn CtrlLogic>> {
        let CtrlKind::OnOff { mode } = mapping.ctrl_kind else {
            return None;
        };

        Some(Box::new(OnOffLogic {
            mode,
            ctrl_in_num: mapping.ctrl_in_num,
            ctrl_out_num: mapping.ctrl_out_num,
            midi: mapping.midi,
            osc_addr: mapping.osc_addr(),
            state: false,
        }))
    }

    fn handle_ctrl(&mut self, num: u8, val: u8) -> Option<Response> {
        let ctrl_in_num = self.ctrl_in_num?;

        if num != ctrl_in_num {
            return None;
        }

        let pressed = val != 0x00;
        let mut new_state = self.state;
        let mut send_ctrl = true;
        let mut send_osc = true;
        let mut remember = true;
        match self.mode {
            OnOffMode::Raw => {
                new_state = pressed;
                send_ctrl = false;
                remember = false;
            }
            OnOffMode::Momentary => {
                new_state = pressed;
            }
            OnOffMode::Toggle => {
                if pressed {
                    new_state = !self.state;
                } else {
                    send_ctrl = false;
                    send_osc = false;
                }
            }
        }

        let mut response = self.update(new_state, remember);

        if !send_ctrl {
            response.ctrl = None;
        }

        if !send_osc {
            response.osc = None;
        }

        Some(response)
    }

    fn handle_osc(&mut self, msg: &OscMessage) -> Option<Response> {
        let _num = self.ctrl_out_num?;

        if msg.addr != self.osc_addr {
            return None;
        }

        if msg.args.is_empty() {
            return None;
        }

        let OscType::Float(val) = msg.args[0] else {
            return None;
        };

        let mut response = Response::new();
        response.ctrl = self.update(val != 0.0, true).ctrl;
        Some(response)
    }

    fn handle_midi(&mut self, msg: &[u8]) -> Option<Response> {
        let _num = self.ctrl_out_num?;

        let midi_spec = self.midi?;

        if msg.len() != 3 {
            return None;
        }

        let status = msg[0];
        let num = msg[1];
        let val = msg[2];

        if status != 0b10110000 | midi_spec.channel {
            return None;
        }

        if num != midi_spec.num {
            return None;
        }

        let mut response = Response::new();
        response.ctrl = self.update(val != 0, true).ctrl;
        Some(response)
    }
}

#[derive(Debug)]
pub struct EightBitLogic {
    ctrl_in_hi_num: u8,
    ctrl_in_lo_num: u8,
    midi: Option<MidiSpec>,
    osc_addr: String,
    state: [u8; 2],
}

impl CtrlLogic for EightBitLogic {
    fn from_mapping(mapping: &Mapping) -> Option<Box<dyn CtrlLogic>> {
        let CtrlKind::EightBit = mapping.ctrl_kind else {
            return None;
        };

        let ctrl_in_sequence = mapping.ctrl_in_sequence.as_ref()?;

        if ctrl_in_sequence.len() < 2 {
            return None;
        }

        Some(Box::new(EightBitLogic {
            ctrl_in_hi_num: ctrl_in_sequence[0],
            ctrl_in_lo_num: ctrl_in_sequence[1],
            midi: mapping.midi,
            osc_addr: format!("/{}", mapping.name),
            state: [0x00, 0x00],
        }))
    }

    fn handle_ctrl(&mut self, num: u8, val: u8) -> Option<Response> {
        if num == self.ctrl_in_hi_num {
            self.state[0] = val;
            return Some(Response::new());
        }

        if num == self.ctrl_in_lo_num {
            self.state[1] = val;
            let val8 = self.state[0] << 1 | (if self.state[1] != 0x00 { 1 } else { 0 });
            return Some(Response {
                ctrl: None,
                osc: Some(OscResponse {
                    addr: self.osc_addr.clone(),
                    args: vec![OscType::Float(val8 as f32 / 255.0)],
                }),
                midi: self.midi.map(|midi| {
                    let data = match midi.kind {
                        MidiKind::Cc => {
                            vec![0b10110000 | midi.channel, midi.num, val8 >> 1]
                        }
                    };
                    MidiResponse { data }
                }),
            });
        }

        None
    }

    fn handle_osc(&mut self, _msg: &OscMessage) -> Option<Response> {
        None
    }

    fn handle_midi(&mut self, _msg: &[u8]) -> Option<Response> {
        None
    }
}

#[derive(Debug)]
pub struct RelativeLogic {
    mode: RelativeMode,
    ctrl_in_num: Option<u8>,
    ctrl_out_num: Option<u8>,
    midi: Option<MidiSpec>,
    osc_addr: String,
    state: u8,
}

impl RelativeLogic {
    fn update(&mut self, new_state: u8) -> Response {
        let changed = new_state != self.state;
        let new_encoder_led_val = Self::encoder_led_val(new_state);
        let encoder_led_val_changed = new_encoder_led_val != Self::encoder_led_val(self.state);
        self.state = new_state;

        if !changed {
            return Response::new();
        }

        let ctrl = if encoder_led_val_changed {
            self.ctrl_out_num.map(|num| CtrlResponse {
                data: vec![num, self.state],
            })
        } else {
            None
        };

        Response {
            ctrl,
            osc: Some(OscResponse {
                addr: self.osc_addr.clone(),
                args: vec![OscType::Float(self.state as f32 / 127.0)],
            }),
            midi: self.midi.map(|midi| {
                let data = match midi.kind {
                    MidiKind::Cc => {
                        vec![0b10110000 | midi.channel, midi.num, self.state]
                    }
                };
                MidiResponse { data }
            }),
        }
    }

    fn encoder_led_val(val: u8) -> u8 {
        if val < 7 { 0 } else { (val - 7) / 11 * 11 + 7 }
    }
}

impl CtrlLogic for RelativeLogic {
    fn from_mapping(mapping: &Mapping) -> Option<Box<dyn CtrlLogic>> {
        let CtrlKind::Relative { mode } = mapping.ctrl_kind else {
            return None;
        };

        Some(Box::new(RelativeLogic {
            mode,
            ctrl_in_num: mapping.ctrl_in_num,
            ctrl_out_num: mapping.ctrl_out_num,
            midi: mapping.midi,
            osc_addr: mapping.osc_addr(),
            state: 0x00,
        }))
    }

    fn handle_ctrl(&mut self, num: u8, val: u8) -> Option<Response> {
        let ctrl_in_num = self.ctrl_in_num?;

        if num != ctrl_in_num {
            return None;
        }

        let delta: i8 = if val < 0x40 {
            val as i8
        } else {
            val as i8 + i8::MIN
        };
        let response = match self.mode {
            RelativeMode::Raw => OscResponse {
                addr: self.osc_addr.clone(),
                args: vec![OscType::Float(delta as f32)],
            }
            .into(),
            RelativeMode::Accumulate => {
                self.update(self.state.saturating_add_signed(delta).min(127))
            }
        };

        Some(response)
    }

    fn handle_osc(&mut self, msg: &OscMessage) -> Option<Response> {
        let _num = self.ctrl_out_num?;

        if msg.addr != self.osc_addr {
            return None;
        }

        if msg.args.is_empty() {
            return None;
        }

        let OscType::Float(val) = msg.args[0] else {
            return None;
        };

        let new_state = float_to_7bit(val);

        let mut response = Response::new();
        response.ctrl = self.update(new_state).ctrl;
        Some(response)
    }

    fn handle_midi(&mut self, msg: &[u8]) -> Option<Response> {
        let _num = self.ctrl_out_num?;

        let midi_spec = self.midi?;

        if msg.len() != 3 {
            return None;
        }

        let status = msg[0];
        let num = msg[1];
        let val = msg[2];

        if status != 0b10110000 | midi_spec.channel {
            return None;
        }

        if num != midi_spec.num {
            return None;
        }

        let mut response = Response::new();
        response.ctrl = self.update(val).ctrl;
        Some(response)
    }
}

#[derive(Debug)]
pub struct CtrlResponse {
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct OscResponse {
    pub addr: String,
    pub args: Vec<OscType>,
}

#[derive(Debug)]
pub struct MidiResponse {
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct Response {
    pub ctrl: Option<CtrlResponse>,
    pub osc: Option<OscResponse>,
    pub midi: Option<MidiResponse>,
}

impl Response {
    pub fn new() -> Response {
        Response {
            ctrl: None,
            osc: None,
            midi: None,
        }
    }
}

impl Default for Response {
    fn default() -> Self {
        Self::new()
    }
}

impl From<CtrlResponse> for Response {
    fn from(val: CtrlResponse) -> Self {
        Response {
            ctrl: Some(val),
            osc: None,
            midi: None,
        }
    }
}

impl From<OscResponse> for Response {
    fn from(val: OscResponse) -> Self {
        Response {
            ctrl: None,
            osc: Some(val),
            midi: None,
        }
    }
}

impl From<MidiResponse> for Response {
    fn from(val: MidiResponse) -> Self {
        Response {
            ctrl: None,
            osc: None,
            midi: Some(val),
        }
    }
}

fn float_to_7bit(val: f32) -> u8 {
    (val.clamp(0.0, 1.0) * 127.0).round() as u8
}

#[cfg(test)]
mod tests {
    use rosc::OscType;

    use super::super::schema::{
        AbstractMapping, Config, CtrlKind, Interface, Mapping, OnOffMode, OscInterface,
        RelativeMode,
    };
    use super::*;

    // — helpers —————————————————————————————————————————————————————————————

    fn make_mapping(ctrl_kind: CtrlKind) -> Mapping {
        Mapping {
            name: "test".into(),
            ctrl_in_num: Some(0x10),
            ctrl_out_num: Some(0x20),
            ctrl_in_sequence: None,
            midi: None,
            ctrl_kind,
        }
    }

    fn osc_float(response: &Response) -> Option<f32> {
        match response.osc.as_ref()?.args.first()? {
            OscType::Float(f) => Some(*f),
            _ => None,
        }
    }

    fn make_config(mappings: Vec<AbstractMapping>) -> Config {
        Config {
            vendor_id: 0x1234,
            product_id: 0x5678,
            in_endpoint: 1,
            out_endpoint: 2,
            interface: Interface::Osc(OscInterface {
                host_addr: "127.0.0.1:9000".parse().unwrap(),
                out_addr: "127.0.0.1:9001".parse().unwrap(),
                in_addr: "127.0.0.1:9002".parse().unwrap(),
            }),
            mappings,
        }
    }

    // — float_to_7bit ————————————————————————————————————————————————————————

    #[test]
    fn float_to_7bit_zero() {
        assert_eq!(float_to_7bit(0.0), 0);
    }

    #[test]
    fn float_to_7bit_one() {
        assert_eq!(float_to_7bit(1.0), 127);
    }

    #[test]
    fn float_to_7bit_half() {
        assert_eq!(float_to_7bit(0.5), 64); // 63.5 rounds to 64
    }

    #[test]
    fn float_to_7bit_clamps() {
        assert_eq!(float_to_7bit(-1.0), 0);
        assert_eq!(float_to_7bit(2.0), 127);
    }

    // — encoder_led_val ——————————————————————————————————————————————————————

    #[test]
    fn encoder_led_val_below_threshold() {
        for v in 0..7 {
            assert_eq!(RelativeLogic::encoder_led_val(v), 0, "val={v}");
        }
    }

    #[test]
    fn encoder_led_val_steps() {
        assert_eq!(RelativeLogic::encoder_led_val(7), 7);
        assert_eq!(RelativeLogic::encoder_led_val(17), 7); // (17-7)/11 = 0 → 0*11+7 = 7
        assert_eq!(RelativeLogic::encoder_led_val(18), 18); // (18-7)/11 = 1 → 1*11+7 = 18
        assert_eq!(RelativeLogic::encoder_led_val(127), 117); // (120)/11 = 10 → 10*11+7 = 117
    }

    // — OnOffLogic ———————————————————————————————————————————————————————————

    #[test]
    fn onoff_raw_sends_osc_not_ctrl() {
        let mut logic = OnOffLogic::from_mapping(&make_mapping(CtrlKind::OnOff {
            mode: OnOffMode::Raw,
        }))
        .unwrap();
        let press = logic.handle_ctrl(0x10, 0x7f).unwrap();
        assert!(press.ctrl.is_none(), "Raw mode must not send ctrl feedback");
        assert_eq!(osc_float(&press), Some(1.0));

        let release = logic.handle_ctrl(0x10, 0x00).unwrap();
        assert!(release.ctrl.is_none());
        assert_eq!(osc_float(&release), Some(0.0));
    }

    #[test]
    fn onoff_momentary_press_and_release() {
        let mut logic = OnOffLogic::from_mapping(&make_mapping(CtrlKind::OnOff {
            mode: OnOffMode::Momentary,
        }))
        .unwrap();

        let press = logic.handle_ctrl(0x10, 0x7f).unwrap();
        assert!(press.ctrl.is_some());
        assert_eq!(osc_float(&press), Some(1.0));

        let release = logic.handle_ctrl(0x10, 0x00).unwrap();
        assert!(release.ctrl.is_some());
        assert_eq!(osc_float(&release), Some(0.0));
    }

    #[test]
    fn onoff_toggle_flips_on_press() {
        let mut logic = OnOffLogic::from_mapping(&make_mapping(CtrlKind::OnOff {
            mode: OnOffMode::Toggle,
        }))
        .unwrap();

        let first = logic.handle_ctrl(0x10, 0x7f).unwrap();
        assert_eq!(osc_float(&first), Some(1.0));
        assert!(first.ctrl.is_some());

        let second = logic.handle_ctrl(0x10, 0x7f).unwrap();
        assert_eq!(osc_float(&second), Some(0.0));
        assert!(second.ctrl.is_some());
    }

    #[test]
    fn onoff_toggle_release_sends_nothing() {
        let mut logic = OnOffLogic::from_mapping(&make_mapping(CtrlKind::OnOff {
            mode: OnOffMode::Toggle,
        }))
        .unwrap();
        logic.handle_ctrl(0x10, 0x7f); // press to set state
        let release = logic.handle_ctrl(0x10, 0x00).unwrap();
        assert!(release.ctrl.is_none());
        assert!(release.osc.is_none());
    }

    #[test]
    fn onoff_wrong_num_returns_none() {
        let mut logic = OnOffLogic::from_mapping(&make_mapping(CtrlKind::OnOff {
            mode: OnOffMode::Momentary,
        }))
        .unwrap();
        assert!(logic.handle_ctrl(0x99, 0x7f).is_none());
    }

    #[test]
    fn onoff_no_ctrl_in_num_returns_none() {
        let mapping = Mapping {
            ctrl_in_num: None,
            ..make_mapping(CtrlKind::OnOff {
                mode: OnOffMode::Momentary,
            })
        };
        let mut logic = OnOffLogic::from_mapping(&mapping).unwrap();
        assert!(logic.handle_ctrl(0x10, 0x7f).is_none());
    }

    #[test]
    fn onoff_midi_wrong_length_returns_none() {
        let mut logic = OnOffLogic::from_mapping(&make_mapping(CtrlKind::OnOff {
            mode: OnOffMode::Momentary,
        }))
        .unwrap();
        assert!(logic.handle_midi(&[0xb0, 0x10]).is_none()); // 2 bytes instead of 3
    }

    #[test]
    fn onoff_osc_non_float_arg_returns_none() {
        use rosc::OscMessage;
        let mut logic = OnOffLogic::from_mapping(&make_mapping(CtrlKind::OnOff {
            mode: OnOffMode::Momentary,
        }))
        .unwrap();
        let msg = OscMessage {
            addr: "/test".into(),
            args: vec![OscType::Int(1)],
        };
        assert!(logic.handle_osc(&msg).is_none());
    }

    // — EightBitLogic ————————————————————————————————————————————————————————

    fn eightbit_mapping() -> Mapping {
        Mapping {
            ctrl_in_sequence: Some(vec![0x10, 0x11]),
            ..make_mapping(CtrlKind::EightBit)
        }
    }

    #[test]
    fn eightbit_hi_byte_returns_empty() {
        let mut logic = EightBitLogic::from_mapping(&eightbit_mapping()).unwrap();
        let response = logic.handle_ctrl(0x10, 0x3f).unwrap();
        assert!(response.osc.is_none(), "hi byte alone should not emit OSC");
    }

    #[test]
    fn eightbit_combined_value() {
        let mut logic = EightBitLogic::from_mapping(&eightbit_mapping()).unwrap();
        logic.handle_ctrl(0x10, 0x7f); // hi = 0x7f
        let response = logic.handle_ctrl(0x11, 0x01).unwrap(); // lo = non-zero
        // val8 = 0x7f << 1 | 1 = 0xff = 255 → osc = 255/255 = 1.0
        let val = osc_float(&response).unwrap();
        assert!((val - 1.0).abs() < 0.01);
    }

    #[test]
    fn eightbit_wrong_num_returns_none() {
        let mut logic = EightBitLogic::from_mapping(&eightbit_mapping()).unwrap();
        assert!(logic.handle_ctrl(0x99, 0x01).is_none());
    }

    #[test]
    fn eightbit_missing_sequence_returns_none() {
        let mapping = make_mapping(CtrlKind::EightBit); // ctrl_in_sequence: None
        assert!(EightBitLogic::from_mapping(&mapping).is_none());
    }

    #[test]
    fn eightbit_short_sequence_returns_none() {
        let mapping = Mapping {
            ctrl_in_sequence: Some(vec![0x10]), // only 1 element
            ..make_mapping(CtrlKind::EightBit)
        };
        assert!(EightBitLogic::from_mapping(&mapping).is_none());
    }

    // — RelativeLogic ————————————————————————————————————————————————————————

    #[test]
    fn relative_accumulate_clockwise() {
        let mut logic = RelativeLogic::from_mapping(&make_mapping(CtrlKind::Relative {
            mode: RelativeMode::Accumulate,
        }))
        .unwrap();
        let response = logic.handle_ctrl(0x10, 0x05).unwrap(); // delta +5
        assert!((osc_float(&response).unwrap() - 5.0 / 127.0).abs() < 0.001);
    }

    #[test]
    fn relative_accumulate_counterclockwise() {
        let mut logic = RelativeLogic::from_mapping(&make_mapping(CtrlKind::Relative {
            mode: RelativeMode::Accumulate,
        }))
        .unwrap();
        logic.handle_ctrl(0x10, 0x0a); // +10
        let response = logic.handle_ctrl(0x10, 0x7e).unwrap(); // -2 (0x7e as i8 + i8::MIN = -2)
        assert!((osc_float(&response).unwrap() - 8.0 / 127.0).abs() < 0.001);
    }

    #[test]
    fn relative_accumulate_clamps_at_zero() {
        let mut logic = RelativeLogic::from_mapping(&make_mapping(CtrlKind::Relative {
            mode: RelativeMode::Accumulate,
        }))
        .unwrap();
        logic.handle_ctrl(0x10, 0x7f); // -1 on state=0, saturates to 0 — no change
        let response = logic.handle_ctrl(0x10, 0x01).unwrap(); // +1 → state=1, not 0
        assert!((osc_float(&response).unwrap() - 1.0 / 127.0).abs() < 0.001);
    }

    #[test]
    fn relative_accumulate_clamps_at_max() {
        let mut logic = RelativeLogic::from_mapping(&make_mapping(CtrlKind::Relative {
            mode: RelativeMode::Accumulate,
        }))
        .unwrap();
        for _ in 0..200 {
            logic.handle_ctrl(0x10, 0x01); // +1 until clamped at 127
        }
        let at_max = logic.handle_ctrl(0x10, 0x01).unwrap(); // still 127, no change
        assert!(at_max.osc.is_none(), "no OSC when value unchanged at max");
    }

    #[test]
    fn relative_raw_passes_delta() {
        let mut logic = RelativeLogic::from_mapping(&make_mapping(CtrlKind::Relative {
            mode: RelativeMode::Raw,
        }))
        .unwrap();
        let cw = logic.handle_ctrl(0x10, 0x03).unwrap(); // delta +3
        assert_eq!(osc_float(&cw), Some(3.0));

        let ccw = logic.handle_ctrl(0x10, 0x7e).unwrap(); // delta -2
        assert_eq!(osc_float(&ccw), Some(-2.0));
    }

    #[test]
    fn relative_delta_at_0x40_boundary() {
        let mut logic = RelativeLogic::from_mapping(&make_mapping(CtrlKind::Relative {
            mode: RelativeMode::Raw,
        }))
        .unwrap();
        // 0x40 = 64; 64 as i8 = 64, + i8::MIN (-128) = -64
        let response = logic.handle_ctrl(0x10, 0x40).unwrap();
        assert_eq!(osc_float(&response), Some(-64.0));
    }

    #[test]
    fn relative_no_ctrl_in_num_returns_none() {
        let mapping = Mapping {
            ctrl_in_num: None,
            ..make_mapping(CtrlKind::Relative {
                mode: RelativeMode::Accumulate,
            })
        };
        let mut logic = RelativeLogic::from_mapping(&mapping).unwrap();
        assert!(logic.handle_ctrl(0x10, 0x7f).is_none());
    }

    #[test]
    fn relative_midi_wrong_length_returns_none() {
        let mut logic = RelativeLogic::from_mapping(&make_mapping(CtrlKind::Relative {
            mode: RelativeMode::Accumulate,
        }))
        .unwrap();
        assert!(logic.handle_midi(&[0xb0, 0x10]).is_none()); // 2 bytes instead of 3
    }

    #[test]
    fn relative_osc_non_float_arg_returns_none() {
        use rosc::OscMessage;
        let mut logic = RelativeLogic::from_mapping(&make_mapping(CtrlKind::Relative {
            mode: RelativeMode::Accumulate,
        }))
        .unwrap();
        let msg = OscMessage {
            addr: "/test".into(),
            args: vec![OscType::Int(1)],
        };
        assert!(logic.handle_osc(&msg).is_none());
    }

    // — Interpreter routing ——————————————————————————————————————————————————

    #[test]
    fn interpreter_routes_known_ctrl_num() {
        let config = make_config(vec![AbstractMapping::Single(make_mapping(
            CtrlKind::OnOff {
                mode: OnOffMode::Momentary,
            },
        ))]);
        let mut interp = Interpreter::new(&config);
        assert!(interp.handle_ctrl(0x10, 0x7f).is_some());
    }

    #[test]
    fn interpreter_returns_none_for_unknown_ctrl_num() {
        let config = make_config(vec![AbstractMapping::Single(make_mapping(
            CtrlKind::OnOff {
                mode: OnOffMode::Momentary,
            },
        ))]);
        let mut interp = Interpreter::new(&config);
        assert!(interp.handle_ctrl(0x99, 0x7f).is_none());
    }
}
