use anyhow::{anyhow, Context, Result};
use goxlr_ipc::{DaemonRequest, DaemonResponse, DeviceStatus, GoXLRCommand, Socket};

#[derive(Debug)]
pub struct Client<'a> {
    socket: Socket<'a, DaemonResponse, DaemonRequest>,
    device: DeviceStatus,
}

impl<'a> Client<'a> {
    pub fn new(socket: Socket<'a, DaemonResponse, DaemonRequest>) -> Self {
        Self {
            socket,
            device: DeviceStatus::default(),
        }
    }

    pub async fn send(&mut self, command: GoXLRCommand) -> Result<()> {
        self.socket
            .send(DaemonRequest::Command(command))
            .await
            .context("Failed to send a command to the GoXLR daemon process")?;
        let result = self
            .socket
            .read()
            .await
            .context("Failed to retrieve the command result from the GoXLR daemon process")?
            .context("Failed to parse the command result from the GoXLR daemon process")?;

        match result {
            DaemonResponse::Ok(Some(device)) => {
                self.device = device;
                Ok(())
            }
            DaemonResponse::Ok(None) => Ok(()),
            DaemonResponse::Error(error) => Err(anyhow!("{}", error)),
        }
    }

    pub fn device(&self) -> &DeviceStatus {
        &self.device
    }
}
