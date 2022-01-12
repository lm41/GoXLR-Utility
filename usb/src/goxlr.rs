use crate::buttonstate::{ButtonStates, Buttons};
use crate::channelstate::ChannelState;
use crate::commands::Command;
use crate::commands::SystemInfoCommand;
use crate::commands::SystemInfoCommand::SupportsDCPCategory;
use crate::dcp::DCPCategory;
use crate::error::ConnectError;
use crate::microphone::MicrophoneType;
use crate::routing::InputDevice;
use byteorder::{ByteOrder, LittleEndian};
use enumset::EnumSet;
use goxlr_types::{ChannelName, FaderName};
use log::{info, warn};
use rusb::{
    Device, DeviceDescriptor, DeviceHandle, Direction, GlobalContext, Language, Recipient,
    RequestType, UsbContext,
};
use std::thread::sleep;
use std::time::Duration;

#[derive(Debug)]
pub struct GoXLR<T: UsbContext> {
    handle: DeviceHandle<T>,
    device: Device<T>,
    device_descriptor: DeviceDescriptor,
    timeout: Duration,
    language: Language,
    command_count: u16,
    device_is_claimed: bool,
}

pub const VID_GOXLR: u16 = 0x1220;
pub const PID_GOXLR_MINI: u16 = 0x8fe4;
pub const PID_GOXLR_FULL: u16 = 0x8fe0;

impl GoXLR<GlobalContext> {
    pub fn open() -> Result<Self, ConnectError> {
        let mut error = ConnectError::DeviceNotFound;
        for device in rusb::devices()?.iter() {
            if let Ok(descriptor) = device.device_descriptor() {
                if descriptor.vendor_id() == VID_GOXLR
                    && (descriptor.product_id() == PID_GOXLR_FULL
                        || descriptor.product_id() == PID_GOXLR_MINI)
                {
                    match device.open() {
                        Ok(handle) => return GoXLR::from_device(handle, descriptor),
                        Err(e) => error = e.into(),
                    }
                }
            }
        }

        Err(error)
    }
}

impl<T: UsbContext> GoXLR<T> {
    pub fn from_device(
        mut handle: DeviceHandle<T>,
        device_descriptor: DeviceDescriptor,
    ) -> Result<Self, ConnectError> {
        let device = handle.device();
        let timeout = Duration::from_secs(1);

        info!("Connected to possible GoXLR device at {:?}", device);

        let languages = handle.read_languages(timeout)?;
        let language = languages
            .get(0)
            .ok_or(ConnectError::DeviceNotGoXLR)?
            .to_owned();

        let _ = handle.set_active_configuration(1);
        let device_is_claimed = handle.claim_interface(0).is_ok();

        let mut goxlr = Self {
            handle,
            device,
            device_descriptor,
            timeout,
            language,
            command_count: 0,
            device_is_claimed,
        };

        goxlr.read_control(RequestType::Vendor, 0, 0, 0, 24)?; // ??

        goxlr.write_control(RequestType::Vendor, 1, 0, 0, &[])?;
        goxlr.read_control(RequestType::Vendor, 3, 0, 0, 1040)?; // ??

        Ok(goxlr)
    }

    pub fn usb_device_descriptor(&self) -> &DeviceDescriptor {
        &self.device_descriptor
    }

    pub fn usb_device_manufacturer(&self) -> Result<String, rusb::Error> {
        self.handle.read_manufacturer_string(
            self.language,
            &self.device_descriptor,
            Duration::from_millis(100),
        )
    }

    pub fn usb_device_product_name(&self) -> Result<String, rusb::Error> {
        self.handle.read_product_string(
            self.language,
            &self.device_descriptor,
            Duration::from_millis(100),
        )
    }

    pub fn usb_device_is_claimed(&self) -> bool {
        self.device_is_claimed
    }

    pub fn usb_device_has_kernel_driver_active(&self) -> Result<bool, rusb::Error> {
        self.handle.kernel_driver_active(0)
    }

    pub fn usb_bus_number(&self) -> u8 {
        self.device.bus_number()
    }

    pub fn usb_address(&self) -> u8 {
        self.device.address()
    }

    pub fn read_control(
        &mut self,
        request_type: RequestType,
        request: u8,
        value: u16,
        index: u16,
        length: usize,
    ) -> Result<Vec<u8>, rusb::Error> {
        let mut buf = vec![0; length];
        let response_length = self.handle.read_control(
            rusb::request_type(Direction::In, request_type, Recipient::Interface),
            request,
            value,
            index,
            &mut buf,
            self.timeout,
        )?;
        buf.truncate(response_length);
        Ok(buf)
    }

    pub fn write_control(
        &mut self,
        request_type: RequestType,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), rusb::Error> {
        self.handle.write_control(
            rusb::request_type(Direction::Out, request_type, Recipient::Interface),
            request,
            value,
            index,
            data,
            self.timeout,
        )?;

        Ok(())
    }

    pub fn request_data(&mut self, command: Command, body: &[u8]) -> Result<Vec<u8>, rusb::Error> {
        self.command_count += 1;
        let command_index = self.command_count;
        let mut full_request = vec![0; 16];
        LittleEndian::write_u32(&mut full_request[0..4], command.command_id());
        LittleEndian::write_u16(&mut full_request[4..6], body.len() as u16);
        LittleEndian::write_u16(&mut full_request[6..8], command_index);
        full_request.extend(body);

        self.write_control(RequestType::Vendor, 2, 0, 0, &full_request)?;

        // TODO: A retry mechanism
        sleep(Duration::from_millis(10));
        self.await_interrupt(Duration::from_secs(2));

        let mut response_header = self.read_control(RequestType::Vendor, 3, 0, 0, 1040)?;
        let response = response_header.split_off(16);
        let response_length = LittleEndian::read_u16(&response_header[4..6]);
        let response_command_index = LittleEndian::read_u16(&response_header[6..8]);

        debug_assert!(response.len() == response_length as usize);
        debug_assert!(response_command_index == command_index);

        Ok(response)
    }

    pub fn supports_dcp_category(&mut self, category: DCPCategory) -> Result<bool, rusb::Error> {
        let mut out = [0; 2];
        LittleEndian::write_u16(&mut out, category.id());
        let result = self.request_data(Command::SystemInfo(SupportsDCPCategory), &out)?;
        Ok(LittleEndian::read_u16(&result) == 1)
    }

    pub fn get_system_info(&mut self) -> Result<(), rusb::Error> {
        let _result =
            self.request_data(Command::SystemInfo(SystemInfoCommand::FirmwareVersion), &[])?;
        // TODO: parse that?
        Ok(())
    }

    pub fn set_fader(&mut self, fader: FaderName, channel: ChannelName) -> Result<(), rusb::Error> {
        // Channel ID, unknown, unknown, unknown
        self.request_data(Command::SetFader(fader), &[channel as u8, 0x00, 0x00, 0x00])?;
        Ok(())
    }

    pub fn set_volume(&mut self, channel: ChannelName, volume: u8) -> Result<(), rusb::Error> {
        self.request_data(Command::SetChannelVolume(channel), &[volume])?;
        Ok(())
    }

    pub fn set_volume_percent(&mut self, channel: Channel, percent: f64) -> Result<(), rusb::Error> {
        self.request_data(Command::SetChannelVolume(channel), &[(0xFF as f64 * percent) as u8])?;
        Ok(())
    }

    pub fn set_channel_state(
        &mut self,
        channel: ChannelName,
        state: ChannelState,
    ) -> Result<(), rusb::Error> {
        self.request_data(Command::SetChannelState(channel), &[state.id()])?;
        Ok(())
    }

    pub fn set_button_states(&mut self, data: [ButtonStates; 24]) -> Result<(), rusb::Error> {
        self.request_data(Command::SetButtonStates(), &data.map(|state| state as u8))?;
        Ok(())
    }

    pub fn set_button_colours(&mut self, data: [u8; 328]) -> Result<(), rusb::Error> {
        self.request_data(Command::SetColourMap(), &data);
        Ok(())
    }

    pub fn set_fader_display_mode(
        &mut self,
        fader: FaderName,
        gradient: bool,
        meter: bool,
    ) -> Result<(), rusb::Error> {
        // This one really doesn't need anything fancy..
        let gradientByte: u8 = if gradient { 0x01 } else { 0x00 };
        let meterByte: u8 = if meter { 0x01 } else { 0x00 };

        // TODO: Seemingly broken?
        self.request_data(
            Command::SetFaderDisplayMode(fader),
            &[gradientByte, meterByte],
        );
        Ok(())
    }

    pub fn set_fader_scribble(
        &mut self,
        fader: FaderName,
        data: [u8; 1024],
    ) -> Result<(), rusb::Error> {
        // Dump it, see what happens..
        self.request_data(Command::SetScribble(fader), &data);
        Ok(())
    }

    pub fn set_routing(
        &mut self,
        input_device: InputDevice,
        data: [u8; 22],
    ) -> Result<(), rusb::Error> {
        self.request_data(Command::SetRouting(input_device), &data)?;
        Ok(())
    }

    pub fn set_microphone_type(
        &mut self,
        microphone_type: MicrophoneType,
        gain: u8,
    ) -> Result<(), rusb::Error> {
        let mut data: [u8; 8] = [0; 8];

        // Before we do *ANYTHING*, we need to reset the mic type..
        self.request_data(Command::SetMicrophoneType(), &data);

        // Set the Microphone Type:
        data[0] = microphone_type.id();
        data[6] = gain;

        self.request_data(Command::SetMicrophoneType(), &data)?;
        Ok(())
    }

    pub fn get_button_states(&mut self) -> Result<(EnumSet<Buttons>, [u8; 4]), rusb::Error> {
        let result = self.request_data(Command::GetButtonStates, &[])?;
        let mut pressed = EnumSet::empty();
        let mut mixers = [0; 4];
        let button_states = LittleEndian::read_u32(&result[0..4]);
        mixers[0] = result[8];
        mixers[1] = result[9];
        mixers[2] = result[10];
        mixers[3] = result[11];

        for button in EnumSet::<Buttons>::all() {
            if button_states & (1 << button as u8) != 0 {
                pressed.insert(button);
            }
        }

        Ok((pressed, mixers))
    }

    pub fn await_interrupt(&mut self, duration: Duration) -> bool {
        let mut buffer = [0u8; 6];
        matches!(
            self.handle.read_interrupt(0x81, &mut buffer, duration),
            Ok(_)
        )
    }
}
