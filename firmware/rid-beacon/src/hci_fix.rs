//! Work around a bt-hci 0.10.1 bug that breaks every extended advertising
//! data command.
//!
//! In bt-hci 0.10.1, `impl WriteHci for &[u8]` writes the slice's length
//! prefix correctly in the blocking `write_hci` (`len`) but wrongly in
//! `write_hci_async` (`len + 1`). Every variable-length HCI parameter is a
//! `&[u8]`, so the async path (which esp-radio's `BleConnector` uses) sends
//! e.g. LE Set Extended Advertising Data with a data-length byte one too
//! large, and the controller rejects it with "Invalid HCI Command
//! Parameters". Verified on the S3: LE Set Extended Advertising Parameters
//! succeeds for legacy, 1M and coded sets, and only the data command fails.
//! Fixed upstream in bt-hci 0.11, but trouble-host 0.8 is pinned to 0.10.
//!
//! The wrapper serialises each outgoing packet with the (correct) blocking
//! encoder into a buffer, then hands the finished bytes to the real
//! transport. Remove it once trouble-host moves to bt-hci >= 0.11.

use bt_hci::transport::{PacketKind, PacketToController, PacketToHost, Transport};
use esp_radio::ble::controller::BleConnector;

/// An HCI command is at most 1 + 3 + 255 bytes; ACL packets are bounded by
/// the packet pool (251). Anything larger falls back to the async encoder.
const MAX_PACKET: usize = 264;

pub struct FixedTransport {
    inner: BleConnector<'static>,
}

impl FixedTransport {
    pub fn new(inner: BleConnector<'static>) -> Self {
        Self { inner }
    }
}

impl embedded_io::ErrorType for FixedTransport {
    type Error = <BleConnector<'static> as embedded_io::ErrorType>::Error;
}

impl Transport for FixedTransport {
    async fn read<'a, P: PacketToHost<'a>>(&self, rx: &'a mut [u8]) -> Result<P, Self::Error> {
        self.inner.read(rx).await
    }

    async fn write<P: PacketToController>(&self, tx: &P) -> Result<(), Self::Error> {
        self.inner.write(&SyncEncoded(tx)).await
    }
}

/// A packet whose async encoding is produced by its blocking encoder.
struct SyncEncoded<'a, P>(&'a P);

impl<P: PacketToController> PacketToController for SyncEncoded<'_, P> {
    const KIND: PacketKind = P::KIND;

    fn size(&self) -> usize {
        self.0.size()
    }

    fn write_hci<W: embedded_io::Write>(&self, writer: W) -> Result<(), W::Error> {
        self.0.write_hci(writer)
    }

    async fn write_hci_async<W: embedded_io_async::Write>(
        &self,
        mut writer: W,
    ) -> Result<(), W::Error> {
        let mut buf = [0u8; MAX_PACKET];
        let total = buf.len();
        let mut rest: &mut [u8] = &mut buf;
        if self.0.write_hci(&mut rest).is_err() {
            // Too big for the buffer: no slice that long exists in our
            // traffic, so the async encoder is as good as anything here.
            return self.0.write_hci_async(writer).await;
        }
        let used = total - rest.len();
        writer.write_all(&buf[..used]).await
    }
}
