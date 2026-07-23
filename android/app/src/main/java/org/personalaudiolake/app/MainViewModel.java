package org.personalaudiolake.app;

import java.io.InputStream;
import java.time.Instant;
import java.time.ZoneId;
import java.time.format.DateTimeFormatter;
import java.util.List;
import java.util.UUID;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.stream.Collectors;
import org.personalaudiolake.app.model.AudioRecorder;
import org.personalaudiolake.app.model.PendingAudio;
import org.personalaudiolake.app.model.PendingAudioStore;
import org.personalaudiolake.app.model.RecordingSender;

/** Renderer-independent recording and encrypted local-transfer state machine. */
public final class MainViewModel {
  public interface Observer {
    void changed(State state);
  }

  public enum Permission {
    MICROPHONE,
    NEARBY_DEVICES
  }

  public record PermissionIntent(UUID id, Permission permission) {}

  public record QueueRow(String id, String name, String timestamp, String status, String detail) {}

  public record State(
      boolean busy,
      boolean recording,
      String status,
      int pendingUploads,
      List<QueueRow> recordings,
      PermissionIntent permissionIntent) {}

  private static final DateTimeFormatter TIME =
      DateTimeFormatter.ofPattern("d MMM, HH:mm").withZone(ZoneId.systemDefault());

  private final ExecutorService executor = Executors.newSingleThreadExecutor();
  private final AudioRecorder recorder;
  private final PendingAudioStore<InputStream> store;
  private final RecordingSender sender;
  private volatile State state;
  private volatile Observer observer;
  private volatile boolean closed;

  public MainViewModel(
      AudioRecorder recorder, PendingAudioStore<InputStream> store, RecordingSender sender) {
    this.recorder = recorder;
    this.store = store;
    this.sender = sender;
    state =
        new State(
            false,
            false,
            store.awaitingUpload().isEmpty()
                ? "Ready to record"
                : "Recordings are waiting for your Mac",
            store.awaitingUpload().size(),
            present(),
            null);
  }

  public void observe(Observer value) {
    observer = value;
    publish(state);
  }

  public void requestRecording() {
    if (!state.busy() && !state.recording())
      permission(Permission.MICROPHONE, "Microphone access is needed to record");
  }

  public void requestSend() {
    if (!state.busy() && !store.awaitingUpload().isEmpty())
      permission(
          Permission.NEARBY_DEVICES,
          "Nearby-device access is needed to send privately to your Mac");
    else if (store.awaitingUpload().isEmpty()) refresh("Nothing is waiting to send");
  }

  private void permission(Permission value, String status) {
    set(
        new State(
            false,
            state.recording(),
            status,
            state.pendingUploads(),
            state.recordings(),
            new PermissionIntent(UUID.randomUUID(), value)));
  }

  public void permissionResult(PermissionIntent intent, boolean granted) {
    if (intent == null || !intent.equals(state.permissionIntent())) return;
    if (!granted) {
      refresh(
          intent.permission() == Permission.MICROPHONE
              ? "Microphone access wasn’t allowed"
              : "Nearby-device access wasn’t allowed");
      return;
    }
    if (intent.permission() == Permission.MICROPHONE) startRecording();
    else send();
  }

  private void startRecording() {
    set(new State(true, false, "Starting…", state.pendingUploads(), state.recordings(), null));
    executor.execute(
        () -> {
          try {
            recorder.start();
            if (closed) {
              recorder.cancel();
              return;
            }
            set(
                new State(
                    false, true, "Recording…", state.pendingUploads(), state.recordings(), null));
          } catch (Exception e) {
            refresh(error(e));
          }
        });
  }

  public void stopRecording() {
    if (state.busy() || !state.recording()) return;
    set(
        new State(
            true, true, "Saving privately…", state.pendingUploads(), state.recordings(), null));
    executor.execute(
        () -> {
          try (AudioRecorder.Capture capture = recorder.stop()) {
            store.stage(
                capture.bytes(),
                capture.displayName(),
                capture.contentType(),
                capture.recordedAtMs());
            set(
                new State(
                    false,
                    false,
                    "Saved privately · ready to send by Bluetooth",
                    store.awaitingUpload().size(),
                    present(),
                    new PermissionIntent(UUID.randomUUID(), Permission.NEARBY_DEVICES)));
          } catch (Exception e) {
            refresh(error(e));
          }
        });
  }

  private void send() {
    set(
        new State(
            true,
            false,
            "Looking for Captain’s Log on your Mac…",
            state.pendingUploads(),
            state.recordings(),
            null));
    executor.execute(
        () -> {
          int sent = 0;
          for (PendingAudio audio : store.awaitingUpload()) {
            store.update(
                audio.id(), PendingAudio.State.UPLOADING, "Advertising privately over Bluetooth");
            set(
                new State(
                    true,
                    false,
                    "Sending " + audio.displayName() + "…",
                    store.awaitingUpload().size(),
                    present(),
                    null));
            try {
              List<RecordingSender.Receipt> receipts = sender.send(List.of(audio));
              if (receipts.size() != 1 || !receipts.get(0).audioId().equals(audio.id()))
                throw new IllegalStateException("Mac returned an invalid transfer receipt");
              RecordingSender.Receipt receipt = receipts.get(0);
              store.complete(receipt.audioId(), receipt.detail());
              sent++;
            } catch (Exception e) {
              store.update(audio.id(), PendingAudio.State.FAILED, error(e));
              refresh(
                  (sent == 0 ? "" : "Sent " + sent + " · ")
                      + error(e)
                      + " · recording kept for retry");
              return;
            }
          }
          refresh(sent == 0 ? "Nothing was sent" : "Sent " + sent + " recording(s) to your Mac");
        });
  }

  public void refreshQueue() {
    refresh(state.status());
  }

  public void close() {
    closed = true;
    executor.shutdownNow();
    recorder.cancel();
    sender.cancel();
  }

  private List<QueueRow> present() {
    return store.recent().stream()
        .map(
            v ->
                new QueueRow(
                    v.id(),
                    v.displayName(),
                    TIME.format(Instant.ofEpochMilli(v.recordedAtMs())),
                    switch (v.state()) {
                      case QUEUED -> "Waiting";
                      case UPLOADING -> "Sending";
                      case UPLOADED -> "Sent";
                      case FAILED -> "Will retry";
                    },
                    v.detail()))
        .collect(Collectors.toList());
  }

  private void refresh(String status) {
    set(
        new State(
            false, state.recording(), status, store.awaitingUpload().size(), present(), null));
  }

  private void set(State value) {
    state = value;
    publish(value);
  }

  private void publish(State value) {
    Observer o = observer;
    if (!closed && o != null) o.changed(value);
  }

  private static String error(Exception e) {
    return e.getMessage() == null || e.getMessage().isBlank()
        ? e.getClass().getSimpleName()
        : e.getMessage();
  }
}
