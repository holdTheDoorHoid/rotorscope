// ROTORSCOPE PATCH (see firmware/patches/README.md): the RF front-end (FE)
// clocks are shared by Wi-Fi, BLE and 802.15.4. ESP-IDF reference-counts them
// (`modem_clock.c`: MODEM_CLOCK_FE is in WIFI_, BLE_ and IEEE802154_CLOCK_DEPS);
// upstream esp-radio gated them whenever any one user disabled its clocks. The
// Wi-Fi blob calls `wifi_clock_disable` around every idle period, so with Wi-Fi
// started the FE clocks were off most of the time and BLE never transmitted.
// Track which users want the FE clocks and gate them only when none does.
use core::sync::atomic::{AtomicU8, Ordering};

static FE_USERS: AtomicU8 = AtomicU8::new(0);
const FE_WIFI: u8 = 1 << 0;
const FE_BT: u8 = 1 << 1;
const FE_IEEE802154: u8 = 1 << 2;

/// Record whether `user` needs the FE clocks; returns whether anyone still does.
fn fe_users_update(user: u8, en: bool) -> bool {
    let prev = if en {
        FE_USERS.fetch_or(user, Ordering::Relaxed)
    } else {
        FE_USERS.fetch_and(!user, Ordering::Relaxed)
    };
    let now = if en { prev | user } else { prev & !user };
    now != 0
}

pub(crate) fn enable_wifi(en: bool) {
    let fe = fe_users_update(FE_WIFI, en);
    regs!(MODEM_SYSCON).clk_conf1().modify(|_, w| {
        w.clk_wifi_apb_en().bit(en);
        w.clk_wifibb_44m_en().bit(en);
        w.clk_wifimac_en().bit(en);
        w.clk_fe_apb_en().bit(fe);
        w.clk_fe_80m_en().bit(fe);
        w.clk_fe_160m_en().bit(fe)
    });
}

pub(crate) fn enable_ieee802154(en: bool) {
    regs!(MODEM_SYSCON).clk_conf().modify(|r, w| {
        w.clk_etm_en().bit(en);
        w.clk_zb_apb_en().bit(en);
        w.clk_zbmac_en().bit(en);
        w.clk_modem_sec_en().bit(en);
        w.clk_modem_sec_ecb_en().bit(en);
        w.clk_modem_sec_ccm_en().bit(en);
        w.clk_modem_sec_bah_en().bit(en);
        w.clk_modem_sec_apb_en().bit(en);
        w.clk_ble_timer_en().bit(en)
    });

    let fe = fe_users_update(FE_IEEE802154, en);
    regs!(MODEM_SYSCON).clk_conf1().modify(|_, w| {
        w.clk_fe_apb_en().bit(fe);
        w.clk_bt_apb_en().bit(en);
        w.clk_btbb_en().bit(en);
        w.clk_btmac_en().bit(en);
        w.clk_fe_20m_en().bit(fe);
        w.clk_fe_40m_en().bit(fe);
        w.clk_fe_80m_en().bit(fe);
        w.clk_fe_160m_en().bit(fe)
    });
}

pub(crate) fn enable_bt(en: bool) {
    regs!(MODEM_SYSCON).clk_conf().modify(|_, w| {
        w.clk_etm_en().bit(en);
        w.clk_modem_sec_en().bit(en);
        w.clk_modem_sec_ecb_en().bit(en);
        w.clk_modem_sec_ccm_en().bit(en);
        w.clk_modem_sec_bah_en().bit(en);
        w.clk_modem_sec_apb_en().bit(en);
        w.clk_ble_timer_en().bit(en)
    });

    let fe = fe_users_update(FE_BT, en);
    regs!(MODEM_SYSCON).clk_conf1().modify(|_, w| {
        w.clk_fe_apb_en().bit(fe);
        w.clk_bt_apb_en().bit(en);
        w.clk_btbb_en().bit(en);
        w.clk_btmac_en().bit(en);
        w.clk_fe_20m_en().bit(fe);
        w.clk_fe_40m_en().bit(fe);
        w.clk_fe_80m_en().bit(fe);
        w.clk_fe_160m_en().bit(fe)
    });
}

pub(crate) fn reset_wifi_mac() {
    // empty
}

pub(crate) fn init_clocks() {
    // done in esp-hal
}

pub(crate) fn deinit_clocks() {
    // nothing to do, `init_clocks` is a no-op
}

pub(crate) fn ble_rtc_clk_init() {
    // nothing for this target (yet)
}

pub(crate) fn reset_rpa() {
    // nothing for this target (yet)
}
