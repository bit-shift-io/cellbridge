# GL.iNet GL-BE9300 (Flint 3) Configuration for CellBridge

To use your GL-BE9300 router as the landing gateway for **CellBridge**, follow these steps to configure the built-in WireGuard server on your router.

---

## 1. Initialize the WireGuard Server

1. Open your browser and log into the GL-BE9300 admin panel (default: `http://192.168.8.1`).
2. In the left sidebar, navigate to **VPN** -> **WireGuard Server**.
3. Under the **Configuration** tab:
   * **Status:** Toggle **ON**.
   * **Listen Port:** `51820` (default).
   * **Interface IP:** Note the tunnel subnet address (usually `10.0.0.1` or `10.1.0.1`).
   * **Endpoint:** Keep track of your router's local IP address (`192.168.8.1:51820`).

---

## 2. Server Advanced Options & Toggles

Under the server options/settings panel, ensure the following parameters are set:

* **Allow Remote Access to the LAN Subnet:** `ENABLED` (Checked)
  * *Crucial so your phone can communicate directly with devices on your `192.168.8.0/24` home network.*
* **IP Masquerading (NAT):** `ENABLED` (Checked)
  * *Ensures proper translation of packets flowing through the tunnel.*
* **MTU:** `1420` (or leave as default/1280)
  * *Prevents packet fragmentation.*
* **Client to Client:** `DISABLED` (Unchecked)
  * *Not required for single-device bridging.*

---

## 3. Route Rules Tab

* **Route Rules Table:** `LEAVE BLANK`
  * *No custom route entries are required on the router. CellBridge handles client-side intercept and routing locally on the Android device.*

---

## 4. Create Client Profile for CellBridge

1. Select the **Profiles** tab under **WireGuard Server**.
2. Click **Add New Profile / User**.
3. Name the profile: `CellBridge` (or `android-bridge`).
4. Save the profile and click **Configurations** to view the generated credentials.
5. Extract the following 4 parameters for your CellBridge app configuration:

| Setting | Example Value | Description |
| :--- | :--- | :--- |
| **`PrivateKey`** | `aB3x...` | Your Android phone's client key |
| **`Address`** | `10.0.0.2/32` | Internal tunnel IP assigned to your phone |
| **`PublicKey`** | `xY9z...` | Flint 3 WireGuard Server Public Key |
| **`Endpoint`** | `192.168.8.1:51820` | Local LAN address of your Flint 3 router |

---

## Technical Summary of Traffic Flow

```
[Android Phone] ──(Cellular Interface)──> [Rust BoringTun Engine]
                                                  │
                                   (Local Wi-Fi UDP Socket)
                                                  │
                                                  ▼
                                     [Flint 3 WireGuard Server]
                                       (192.168.8.1:51820)
                                                  │
                                                  ▼
                                     [Home LAN Subnet Bridge]
```