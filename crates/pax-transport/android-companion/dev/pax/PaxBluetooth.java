package dev.pax;

import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;

import java.util.ArrayList;
import java.util.concurrent.ConcurrentLinkedQueue;

/**
 * Companion class for the `pax` Android backend's live device discovery.
 *
 * Android only delivers classic-inquiry results asynchronously, via
 * {@code BluetoothDevice.ACTION_FOUND} broadcasts — there is no synchronous
 * "list nearby devices" call. The Rust side cannot subclass {@link BroadcastReceiver}
 * over JNI, so this tiny helper does it: it buffers found devices into a queue that
 * Rust drains by polling. No native callbacks (no {@code RegisterNatives}) are
 * required — Rust only calls these three static methods.
 *
 * Bundle this class in your app (e.g. drop it under {@code app/src/main/java/dev/pax/}).
 * Your app must hold the {@code BLUETOOTH_SCAN} (API 31+) or {@code BLUETOOTH_ADMIN}
 * plus location permissions at runtime before calling {@link #startDiscovery}.
 *
 * Usage from Rust mirrors:
 * <pre>
 *   PaxBluetooth.startDiscovery(context);
 *   // ... wait ...
 *   String[] found = PaxBluetooth.drain();   // "AA:BB:CC:DD:EE:FF|Name|-57" lines
 *   PaxBluetooth.stopDiscovery(context);
 * </pre>
 */
public final class PaxBluetooth {
    private static final ConcurrentLinkedQueue<String> FOUND = new ConcurrentLinkedQueue<>();
    private static BroadcastReceiver receiver;

    private PaxBluetooth() {}

    /** Register the receiver and start a classic inquiry. Idempotent-ish. */
    public static synchronized void startDiscovery(Context context) {
        if (receiver == null) {
            receiver = new BroadcastReceiver() {
                @Override
                public void onReceive(Context ctx, Intent intent) {
                    if (BluetoothDevice.ACTION_FOUND.equals(intent.getAction())) {
                        BluetoothDevice device =
                                intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE);
                        if (device == null) {
                            return;
                        }
                        short rssi = intent.getShortExtra(
                                BluetoothDevice.EXTRA_RSSI, Short.MIN_VALUE);
                        String addr = device.getAddress();
                        String name = device.getName();
                        if (name == null) {
                            name = "";
                        }
                        // Pipe-delimited so the Rust side can split cheaply.
                        FOUND.add(addr + "|" + name + "|" + rssi);
                    }
                }
            };
            IntentFilter filter = new IntentFilter(BluetoothDevice.ACTION_FOUND);
            context.registerReceiver(receiver, filter);
        }
        BluetoothAdapter adapter = BluetoothAdapter.getDefaultAdapter();
        if (adapter != null) {
            if (adapter.isDiscovering()) {
                adapter.cancelDiscovery();
            }
            adapter.startDiscovery();
        }
    }

    /**
     * Remove and return everything found since the last drain, as
     * {@code "address|name|rssi"} strings.
     */
    public static String[] drain() {
        ArrayList<String> out = new ArrayList<>();
        String item;
        while ((item = FOUND.poll()) != null) {
            out.add(item);
        }
        return out.toArray(new String[0]);
    }

    /** Cancel the inquiry and unregister the receiver. */
    public static synchronized void stopDiscovery(Context context) {
        BluetoothAdapter adapter = BluetoothAdapter.getDefaultAdapter();
        if (adapter != null && adapter.isDiscovering()) {
            adapter.cancelDiscovery();
        }
        if (receiver != null) {
            try {
                context.unregisterReceiver(receiver);
            } catch (IllegalArgumentException ignored) {
                // Already unregistered.
            }
            receiver = null;
        }
    }
}
