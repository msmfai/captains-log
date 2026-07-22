package org.personalaudiolake.app.adapters;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

import java.util.UUID;
import org.junit.Test;
import org.personalaudiolake.app.model.PendingAudio;

public final class BluetoothTransferProtocolTest {
  private static final String VECTOR =
      "434c423101"
          + "00112233445566778899aabbccddeeff"
          + "102132435465768798a9bacbdcedfe0f"
          + "0000000000000005"
          + "0000018bcfe5687b"
          + "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

  @Test
  public void manifest_matches_the_mac_compatibility_vector() {
    byte[] hash = new byte[32];
    for (int index = 0; index < hash.length; index++) hash[index] = (byte) index;
    PendingAudio audio =
        new PendingAudio(
            "10213243-5465-7687-98a9-bacbdcedfe0f",
            "/private/a.m4a",
            "a.m4a",
            "audio/mp4",
            "android-recorder",
            1_700_000_000_123L,
            5,
            PendingAudio.State.QUEUED,
            "waiting");

    assertEquals(
        VECTOR,
        hex(
            BluetoothTransferProtocol.manifest(
                UUID.fromString("00112233-4455-6677-8899-aabbccddeeff"), audio, hash)));
  }

  @Test
  public void start_data_and_commit_match_the_mac_compatibility_vectors() {
    UUID recording = UUID.fromString("10213243-5465-7687-98a9-bacbdcedfe0f");
    BluetoothTransferProtocol.Start start =
        BluetoothTransferProtocol.parseStart(
            bytes("01" + "102132435465768798a9bacbdcedfe0f" + "0000000000000102"));
    assertEquals(recording, start.recordingId());
    assertEquals(258, start.offset());

    assertEquals(
        "010000000000000102616263",
        hex(BluetoothTransferProtocol.dataFrame(258, new byte[] {'a', 'b', 'c'}, 3)));

    byte[] hash = new byte[32];
    for (int index = 0; index < hash.length; index++) hash[index] = (byte) index;
    assertEquals(
        recording,
        BluetoothTransferProtocol.parseCommit(
            bytes(
                "02"
                    + "102132435465768798a9bacbdcedfe0f"
                    + "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
            hash));
  }

  @Test
  public void malformed_control_and_oversized_manifest_fail_closed() {
    assertThrows(
        IllegalArgumentException.class, () -> BluetoothTransferProtocol.parseStart(new byte[] {1}));
    assertThrows(
        IllegalArgumentException.class,
        () -> BluetoothTransferProtocol.dataFrame(-1, new byte[] {1}, 1));
    PendingAudio oversized =
        new PendingAudio(
            "10213243-5465-7687-98a9-bacbdcedfe0f",
            "/private/a.m4a",
            "a.m4a",
            "audio/mp4",
            "android-recorder",
            1_700_000_000_123L,
            BluetoothTransferProtocol.MAX_RECORDING_BYTES + 1,
            PendingAudio.State.QUEUED,
            "waiting");
    assertThrows(
        IllegalArgumentException.class,
        () ->
            BluetoothTransferProtocol.manifest(
                UUID.fromString("00112233-4455-6677-8899-aabbccddeeff"), oversized, new byte[32]));
  }

  @Test
  public void payload_size_is_safe_before_and_after_mtu_negotiation() {
    assertEquals(11, BluetoothTransferProtocol.payloadBytesForMtu(23));
    assertEquals(173, BluetoothTransferProtocol.payloadBytesForMtu(185));
    assertEquals(480, BluetoothTransferProtocol.payloadBytesForMtu(517));
    assertThrows(
        IllegalArgumentException.class, () -> BluetoothTransferProtocol.payloadBytesForMtu(22));
  }

  private static byte[] bytes(String value) {
    byte[] result = new byte[value.length() / 2];
    for (int index = 0; index < result.length; index++) {
      result[index] = (byte) Integer.parseInt(value.substring(index * 2, index * 2 + 2), 16);
    }
    return result;
  }

  private static String hex(byte[] value) {
    StringBuilder result = new StringBuilder(value.length * 2);
    for (byte item : value) result.append(String.format("%02x", item & 0xff));
    return result.toString();
  }
}
