package org.personalaudiolake.app.adapters;

import java.util.Arrays;
import java.util.UUID;
import org.personalaudiolake.app.model.PendingAudio;

/** Pure wire codec shared by the Android GATT leaf and compatibility-vector tests. */
final class BluetoothTransferProtocol {
  static final long MAX_RECORDING_BYTES = 4L * 1024 * 1024 * 1024;
  static final UUID SERVICE_UUID = UUID.fromString("325ed7d9-14d2-4bae-ac92-ebf257933eb2");
  static final UUID CONTROL_UUID = UUID.fromString("325ed7da-14d2-4bae-ac92-ebf257933eb2");
  static final UUID MANIFEST_UUID = UUID.fromString("325ed7db-14d2-4bae-ac92-ebf257933eb2");
  static final UUID DATA_UUID = UUID.fromString("325ed7dc-14d2-4bae-ac92-ebf257933eb2");
  static final UUID CCC_UUID = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb");

  private BluetoothTransferProtocol() {}

  record Start(UUID recordingId, long offset) {}

  static byte[] manifest(UUID deviceId, PendingAudio audio, byte[] hash) {
    if (hash.length != 32
        || audio.byteLength() <= 0
        || audio.byteLength() > MAX_RECORDING_BYTES
        || audio.recordedAtMs() <= 0) {
      throw new IllegalArgumentException("Invalid Bluetooth recording manifest");
    }
    byte[] value = new byte[85];
    System.arraycopy(new byte[] {'C', 'L', 'B', '1', 1}, 0, value, 0, 5);
    putUuid(value, 5, deviceId);
    putUuid(value, 21, UUID.fromString(audio.id()));
    putNumber(value, 37, audio.byteLength());
    putNumber(value, 45, audio.recordedAtMs());
    System.arraycopy(hash, 0, value, 53, 32);
    return value;
  }

  static Start parseStart(byte[] value) {
    if (value == null || value.length != 25 || value[0] != 1) {
      throw new IllegalArgumentException("Invalid Bluetooth start command");
    }
    long offset = number(value, 17);
    if (offset < 0) throw new IllegalArgumentException("Invalid Bluetooth start offset");
    return new Start(uuid(value, 1), offset);
  }

  static UUID parseCommit(byte[] value, byte[] expectedHash) {
    if (value == null
        || value.length != 49
        || value[0] != 2
        || expectedHash == null
        || expectedHash.length != 32
        || !Arrays.equals(expectedHash, Arrays.copyOfRange(value, 17, 49))) {
      throw new IllegalArgumentException("Invalid Bluetooth commit command");
    }
    return uuid(value, 1);
  }

  static byte[] dataFrame(long offset, byte[] payload, int count) {
    if (offset < 0 || payload == null || count <= 0 || count > payload.length) {
      throw new IllegalArgumentException("Invalid Bluetooth data frame");
    }
    byte[] frame = new byte[9 + count];
    frame[0] = 1;
    putNumber(frame, 1, offset);
    System.arraycopy(payload, 0, frame, 9, count);
    return frame;
  }

  static int payloadBytesForMtu(int mtu) {
    if (mtu < 23) throw new IllegalArgumentException("Invalid Bluetooth ATT MTU");
    return Math.min(480, mtu - 12);
  }

  static UUID uuid(byte[] value, int offset) {
    return new UUID(number(value, offset), number(value, offset + 8));
  }

  static long number(byte[] value, int offset) {
    long result = 0;
    for (int index = 0; index < 8; index++) {
      result = (result << 8) | (value[offset + index] & 0xffL);
    }
    return result;
  }

  static void putNumber(byte[] value, int offset, long number) {
    for (int index = 7; index >= 0; index--) {
      value[offset + index] = (byte) number;
      number >>>= 8;
    }
  }

  private static void putUuid(byte[] value, int offset, UUID id) {
    putNumber(value, offset, id.getMostSignificantBits());
    putNumber(value, offset + 8, id.getLeastSignificantBits());
  }
}
