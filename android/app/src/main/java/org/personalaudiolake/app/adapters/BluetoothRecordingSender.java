package org.personalaudiolake.app.adapters;

import android.Manifest;
import android.annotation.SuppressLint;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattDescriptor;
import android.bluetooth.BluetoothGattServer;
import android.bluetooth.BluetoothGattServerCallback;
import android.bluetooth.BluetoothGattService;
import android.bluetooth.BluetoothManager;
import android.bluetooth.BluetoothProfile;
import android.bluetooth.BluetoothStatusCodes;
import android.bluetooth.le.AdvertiseCallback;
import android.bluetooth.le.AdvertiseData;
import android.bluetooth.le.AdvertiseSettings;
import android.bluetooth.le.BluetoothLeAdvertiser;
import android.content.Context;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.ParcelUuid;
import java.io.File;
import java.io.FileInputStream;
import java.io.RandomAccessFile;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.UUID;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;
import org.personalaudiolake.app.model.PendingAudio;
import org.personalaudiolake.app.model.RecordingSender;

/** Authenticated encrypted GATT leaf. The Mac initiates and verifies each resumable transfer. */
@SuppressLint("MissingPermission")
public final class BluetoothRecordingSender implements RecordingSender {
  private static final int ATT_DEFAULT_MTU = 23;
  private static final UUID SERVICE_UUID = BluetoothTransferProtocol.SERVICE_UUID;
  private static final UUID CONTROL_UUID = BluetoothTransferProtocol.CONTROL_UUID;
  private static final UUID MANIFEST_UUID = BluetoothTransferProtocol.MANIFEST_UUID;
  private static final UUID DATA_UUID = BluetoothTransferProtocol.DATA_UUID;
  private static final UUID CCC_UUID = BluetoothTransferProtocol.CCC_UUID;

  private final Context context;
  private final UUID deviceId;
  private final Semaphore notificationSent = new Semaphore(0);
  private volatile BluetoothGattServer server;
  private volatile BluetoothLeAdvertiser advertiser;
  private volatile BluetoothDevice central;
  private volatile BluetoothGattCharacteristic dataCharacteristic;
  private volatile Transfer current;
  private volatile boolean cancelled;
  private volatile int mtu = ATT_DEFAULT_MTU;
  private volatile int notificationStatus = BluetoothGatt.GATT_FAILURE;
  private volatile CountDownLatch serviceReady;
  private volatile int serviceStatus = BluetoothGatt.GATT_FAILURE;
  private volatile CountDownLatch advertisingReady;
  private volatile int advertisingStatus = AdvertiseCallback.ADVERTISE_FAILED_INTERNAL_ERROR;

  public BluetoothRecordingSender(Context context) {
    this.context = context.getApplicationContext();
    String saved =
        context
            .getSharedPreferences("bluetooth-source", Context.MODE_PRIVATE)
            .getString("device-id", "");
    UUID parsed;
    try {
      parsed = UUID.fromString(saved);
    } catch (Exception ignored) {
      parsed = UUID.randomUUID();
    }
    deviceId = parsed;
    context
        .getSharedPreferences("bluetooth-source", Context.MODE_PRIVATE)
        .edit()
        .putString("device-id", deviceId.toString())
        .apply();
  }

  @Override
  public List<Receipt> send(List<PendingAudio> recordings) throws Exception {
    requirePermissions();
    cancelled = false;
    openServer();
    List<Receipt> receipts = new ArrayList<>();
    try {
      for (PendingAudio audio : recordings) {
        if (cancelled) throw new InterruptedException("Bluetooth send cancelled");
        Transfer transfer = new Transfer(audio, sha256(new File(audio.locator())));
        current = transfer;
        startAdvertising();
        if (!transfer.committed.await(10, TimeUnit.MINUTES)) {
          throw new IllegalStateException(
              "The Mac did not finish receiving " + audio.displayName());
        }
        receipts.add(new Receipt(audio.id(), "Sent to Captain’s Log on this Mac"));
        stopAdvertising();
        central = null;
      }
      return receipts;
    } finally {
      current = null;
      stopAdvertising();
      closeServer();
    }
  }

  private void requirePermissions() {
    if (Build.VERSION.SDK_INT >= 31
        && (context.checkSelfPermission(Manifest.permission.BLUETOOTH_ADVERTISE)
                != PackageManager.PERMISSION_GRANTED
            || context.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT)
                != PackageManager.PERMISSION_GRANTED)) {
      throw new SecurityException("Nearby-device access is needed to send by Bluetooth");
    }
  }

  private void openServer() throws Exception {
    BluetoothManager manager = context.getSystemService(BluetoothManager.class);
    if (manager == null) throw new IllegalStateException("Bluetooth is unavailable");
    BluetoothAdapter adapter = manager.getAdapter();
    if (adapter == null || !adapter.isEnabled())
      throw new IllegalStateException("Turn Bluetooth on and try again");
    advertiser = adapter.getBluetoothLeAdvertiser();
    if (advertiser == null)
      throw new IllegalStateException("This phone cannot advertise Bluetooth Low Energy");
    server = manager.openGattServer(context, callback);
    if (server == null)
      throw new IllegalStateException("Android could not open its Bluetooth transfer service");

    BluetoothGattService service =
        new BluetoothGattService(SERVICE_UUID, BluetoothGattService.SERVICE_TYPE_PRIMARY);
    BluetoothGattCharacteristic manifest =
        new BluetoothGattCharacteristic(
            MANIFEST_UUID,
            BluetoothGattCharacteristic.PROPERTY_READ,
            BluetoothGattCharacteristic.PERMISSION_READ_ENCRYPTED_MITM);
    BluetoothGattCharacteristic control =
        new BluetoothGattCharacteristic(
            CONTROL_UUID,
            BluetoothGattCharacteristic.PROPERTY_WRITE,
            BluetoothGattCharacteristic.PERMISSION_WRITE_ENCRYPTED_MITM);
    dataCharacteristic =
        new BluetoothGattCharacteristic(
            DATA_UUID,
            BluetoothGattCharacteristic.PROPERTY_NOTIFY,
            BluetoothGattCharacteristic.PERMISSION_READ_ENCRYPTED_MITM);
    dataCharacteristic.addDescriptor(
        new BluetoothGattDescriptor(
            CCC_UUID,
            BluetoothGattDescriptor.PERMISSION_READ_ENCRYPTED_MITM
                | BluetoothGattDescriptor.PERMISSION_WRITE_ENCRYPTED_MITM));
    service.addCharacteristic(manifest);
    service.addCharacteristic(control);
    service.addCharacteristic(dataCharacteristic);
    serviceReady = new CountDownLatch(1);
    serviceStatus = BluetoothGatt.GATT_FAILURE;
    if (!server.addService(service))
      throw new IllegalStateException("Android rejected the Bluetooth transfer service");
    if (!serviceReady.await(5, TimeUnit.SECONDS) || serviceStatus != BluetoothGatt.GATT_SUCCESS) {
      throw new IllegalStateException("Android could not publish its Bluetooth transfer service");
    }
  }

  private void startAdvertising() throws Exception {
    AdvertiseSettings settings =
        new AdvertiseSettings.Builder()
            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
            .setConnectable(true)
            .setTimeout(0)
            .build();
    AdvertiseData data =
        new AdvertiseData.Builder()
            .addServiceUuid(new ParcelUuid(SERVICE_UUID))
            .setIncludeDeviceName(false)
            .build();
    advertisingReady = new CountDownLatch(1);
    advertisingStatus = AdvertiseCallback.ADVERTISE_FAILED_INTERNAL_ERROR;
    advertiser.startAdvertising(settings, data, advertiseCallback);
    if (!advertisingReady.await(5, TimeUnit.SECONDS) || advertisingStatus != 0) {
      throw new IllegalStateException(
          "Android could not advertise the recording over Bluetooth (" + advertisingStatus + ")");
    }
  }

  private final AdvertiseCallback advertiseCallback =
      new AdvertiseCallback() {
        @Override
        public void onStartSuccess(AdvertiseSettings settings) {
          advertisingStatus = 0;
          CountDownLatch ready = advertisingReady;
          if (ready != null) ready.countDown();
        }

        @Override
        public void onStartFailure(int code) {
          advertisingStatus = code;
          CountDownLatch ready = advertisingReady;
          if (ready != null) ready.countDown();
        }
      };

  private final BluetoothGattServerCallback callback =
      new BluetoothGattServerCallback() {
        @Override
        public void onServiceAdded(int status, BluetoothGattService service) {
          serviceStatus = status;
          CountDownLatch ready = serviceReady;
          if (ready != null) ready.countDown();
        }

        @Override
        public void onConnectionStateChange(BluetoothDevice device, int status, int newState) {
          if (status == BluetoothGatt.GATT_SUCCESS
              && newState == BluetoothProfile.STATE_CONNECTED) {
            BluetoothDevice active = central;
            if (active == null || active.equals(device)) {
              mtu = ATT_DEFAULT_MTU;
              central = device;
            } else if (server != null) server.cancelConnection(device);
          } else if (central != null && central.equals(device)) central = null;
        }

        @Override
        public void onMtuChanged(BluetoothDevice device, int value) {
          mtu = value;
        }

        @Override
        public void onCharacteristicReadRequest(
            BluetoothDevice device,
            int requestId,
            int offset,
            BluetoothGattCharacteristic characteristic) {
          Transfer transfer = current;
          if (!isActiveCentral(device)
              || !characteristic.getUuid().equals(MANIFEST_UUID)
              || transfer == null) {
            server.sendResponse(device, requestId, BluetoothGatt.GATT_FAILURE, offset, null);
            return;
          }
          byte[] value = manifest(transfer);
          if (offset < 0 || offset > value.length) {
            server.sendResponse(device, requestId, BluetoothGatt.GATT_INVALID_OFFSET, offset, null);
          } else {
            server.sendResponse(
                device,
                requestId,
                BluetoothGatt.GATT_SUCCESS,
                offset,
                Arrays.copyOfRange(value, offset, value.length));
          }
        }

        @Override
        public void onDescriptorWriteRequest(
            BluetoothDevice device,
            int requestId,
            BluetoothGattDescriptor descriptor,
            boolean preparedWrite,
            boolean responseNeeded,
            int offset,
            byte[] value) {
          boolean valid =
              isActiveCentral(device)
                  && descriptor.getUuid().equals(CCC_UUID)
                  && !preparedWrite
                  && offset == 0
                  && Arrays.equals(value, BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE);
          if (valid) descriptor.setValue(value);
          if (responseNeeded)
            server.sendResponse(
                device,
                requestId,
                valid ? BluetoothGatt.GATT_SUCCESS : BluetoothGatt.GATT_FAILURE,
                offset,
                valid ? value : null);
        }

        @Override
        public void onCharacteristicWriteRequest(
            BluetoothDevice device,
            int requestId,
            BluetoothGattCharacteristic characteristic,
            boolean preparedWrite,
            boolean responseNeeded,
            int offset,
            byte[] value) {
          boolean valid =
              isActiveCentral(device)
                  && characteristic.getUuid().equals(CONTROL_UUID)
                  && !preparedWrite
                  && offset == 0
                  && handleControl(device, value);
          int status = valid ? BluetoothGatt.GATT_SUCCESS : BluetoothGatt.GATT_FAILURE;
          if (responseNeeded) server.sendResponse(device, requestId, status, offset, null);
        }

        @Override
        public void onNotificationSent(BluetoothDevice device, int status) {
          if (!isActiveCentral(device)) return;
          notificationStatus = status;
          notificationSent.release();
        }
      };

  private boolean isActiveCentral(BluetoothDevice device) {
    BluetoothDevice active = central;
    return active != null && active.equals(device);
  }

  private boolean handleControl(BluetoothDevice device, byte[] value) {
    Transfer transfer = current;
    if (transfer == null || value == null || value.length == 0) return false;
    UUID expected = UUID.fromString(transfer.audio.id());
    try {
      if (value[0] == 1) {
        BluetoothTransferProtocol.Start start = BluetoothTransferProtocol.parseStart(value);
        if (!start.recordingId().equals(expected) || start.offset() > transfer.audio.byteLength()) {
          return false;
        }
        new Thread(() -> stream(device, transfer, start.offset()), "captains-log-bluetooth-send")
            .start();
        return true;
      }
      if (value[0] == 2) {
        if (!BluetoothTransferProtocol.parseCommit(value, transfer.hash).equals(expected))
          return false;
        transfer.committed.countDown();
        return true;
      }
    } catch (IllegalArgumentException ignored) {
      return false;
    }
    return false;
  }

  private void stream(BluetoothDevice device, Transfer transfer, long offset) {
    try (RandomAccessFile file = new RandomAccessFile(transfer.audio.locator(), "r")) {
      file.seek(offset);
      byte[] payload = new byte[BluetoothTransferProtocol.payloadBytesForMtu(mtu)];
      for (int count; !cancelled && (count = file.read(payload)) != -1; ) {
        byte[] frame = BluetoothTransferProtocol.dataFrame(offset, payload, count);
        notificationSent.drainPermits();
        notificationStatus = BluetoothGatt.GATT_FAILURE;
        if (!notify(device, frame)
            || !notificationSent.tryAcquire(15, TimeUnit.SECONDS)
            || notificationStatus != BluetoothGatt.GATT_SUCCESS) {
          throw new IllegalStateException("Bluetooth notification stalled");
        }
        offset += count;
      }
    } catch (Exception ignored) {
      if (server != null) server.cancelConnection(device);
    }
  }

  private boolean notify(BluetoothDevice device, byte[] value) {
    if (Build.VERSION.SDK_INT >= 33) {
      return server.notifyCharacteristicChanged(device, dataCharacteristic, false, value)
          == BluetoothStatusCodes.SUCCESS;
    }
    dataCharacteristic.setValue(value);
    return server.notifyCharacteristicChanged(device, dataCharacteristic, false);
  }

  private byte[] manifest(Transfer transfer) {
    return BluetoothTransferProtocol.manifest(deviceId, transfer.audio, transfer.hash);
  }

  private static byte[] sha256(File file) throws Exception {
    MessageDigest digest = MessageDigest.getInstance("SHA-256");
    try (FileInputStream input = new FileInputStream(file)) {
      byte[] buffer = new byte[128 * 1024];
      for (int count; (count = input.read(buffer)) != -1; ) digest.update(buffer, 0, count);
    }
    return digest.digest();
  }

  private void stopAdvertising() {
    try {
      if (advertiser != null) advertiser.stopAdvertising(advertiseCallback);
    } catch (SecurityException ignored) {
    }
  }

  private void closeServer() {
    BluetoothGattServer open = server;
    server = null;
    try {
      if (open != null) {
        open.clearServices();
        open.close();
      }
    } catch (SecurityException ignored) {
    }
  }

  @Override
  public void cancel() {
    cancelled = true;
    stopAdvertising();
    closeServer();
  }

  private record Transfer(PendingAudio audio, byte[] hash, CountDownLatch committed) {
    private Transfer(PendingAudio audio, byte[] hash) {
      this(audio, hash, new CountDownLatch(1));
    }
  }
}
