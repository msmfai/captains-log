package org.personalaudiolake.app;

import static org.junit.Assert.*;

import java.io.*;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.*;
import org.junit.Test;
import org.personalaudiolake.app.model.*;

public final class MainViewModelTest {
  @Test
  public void stop_is_durable_before_bluetooth_permission_and_commit() throws Exception {
    Store store = new Store();
    Sender sender = new Sender();
    MainViewModel model = new MainViewModel(new Recorder(), store, sender);
    AtomicReference<MainViewModel.State> state = new AtomicReference<>();
    CountDownLatch recording = new CountDownLatch(1),
        saved = new CountDownLatch(1),
        sent = new CountDownLatch(1);
    model.observe(
        v -> {
          state.set(v);
          if (v.recording()) recording.countDown();
          if (v.permissionIntent() != null
              && v.permissionIntent().permission() == MainViewModel.Permission.NEARBY_DEVICES)
            saved.countDown();
          if (v.status().startsWith("Sent")) sent.countDown();
        });
    model.requestRecording();
    model.permissionResult(state.get().permissionIntent(), true);
    assertTrue(recording.await(2, TimeUnit.SECONDS));
    model.stopRecording();
    assertTrue(saved.await(2, TimeUnit.SECONDS));
    assertEquals(1, store.awaitingUpload().size());
    model.permissionResult(state.get().permissionIntent(), true);
    assertTrue(sent.await(2, TimeUnit.SECONDS));
    assertEquals(0, store.awaitingUpload().size());
    assertEquals(1, sender.calls);
    model.close();
  }

  @Test
  public void denied_nearby_access_keeps_recording_for_retry() {
    Store store = new Store();
    try {
      store.stage(new ByteArrayInputStream(new byte[] {1}), "a.m4a", "audio/mp4", 10);
    } catch (Exception e) {
      fail();
    }
    MainViewModel model = new MainViewModel(new Recorder(), store, new Sender());
    AtomicReference<MainViewModel.State> state = new AtomicReference<>();
    model.observe(state::set);
    model.requestSend();
    model.permissionResult(state.get().permissionIntent(), false);
    assertEquals(1, store.awaitingUpload().size());
    assertTrue(state.get().status().contains("wasn’t allowed"));
    model.close();
  }

  @Test
  public void completed_recordings_commit_before_a_later_transfer_failure() throws Exception {
    Store store = new Store();
    PendingAudio first =
        store.stage(new ByteArrayInputStream(new byte[] {1}), "first.m4a", "audio/mp4", 10);
    PendingAudio second =
        store.stage(new ByteArrayInputStream(new byte[] {2}), "second.m4a", "audio/mp4", 20);
    MainViewModel model = new MainViewModel(new Recorder(), store, new FailingSecondSender());
    AtomicReference<MainViewModel.State> state = new AtomicReference<>();
    CountDownLatch finished = new CountDownLatch(1);
    model.observe(
        v -> {
          state.set(v);
          if (v.status().contains("kept for retry")) finished.countDown();
        });
    model.requestSend();
    model.permissionResult(state.get().permissionIntent(), true);
    assertTrue(finished.await(2, TimeUnit.SECONDS));
    assertEquals(PendingAudio.State.UPLOADED, store.find(first.id()).state());
    assertEquals(PendingAudio.State.FAILED, store.find(second.id()).state());
    assertEquals(1, store.awaitingUpload().size());
    model.close();
  }

  private static final class Recorder implements AudioRecorder {
    public void start() {}

    public Capture stop() {
      return new Capture(new ByteArrayInputStream(new byte[] {1, 2}), "a.m4a", "audio/mp4", 10);
    }

    public void cancel() {}
  }

  private static final class Sender implements RecordingSender {
    int calls;

    public List<Receipt> send(List<PendingAudio> values) {
      calls++;
      return values.stream().map(v -> new Receipt(v.id(), "sent")).toList();
    }

    public void cancel() {}
  }

  private static final class FailingSecondSender implements RecordingSender {
    int calls;

    public List<Receipt> send(List<PendingAudio> values) {
      calls++;
      if (calls == 2) throw new IllegalStateException("radio interrupted");
      return List.of(new Receipt(values.get(0).id(), "sent"));
    }

    public void cancel() {}
  }

  private static final class Store implements PendingAudioStore<InputStream> {
    List<PendingAudio> values = new ArrayList<>();

    public PendingAudio stage(InputStream in, String name, String type, long time)
        throws Exception {
      while (in.read() != -1) {}
      PendingAudio v =
          new PendingAudio(
              UUID.randomUUID().toString(),
              "memory",
              name,
              type,
              "android-recorder",
              time,
              2,
              PendingAudio.State.QUEUED,
              "waiting");
      values.add(v);
      return v;
    }

    public List<PendingAudio> awaitingUpload() {
      return values.stream().filter(v -> v.state() != PendingAudio.State.UPLOADED).toList();
    }

    public List<PendingAudio> recent() {
      return List.copyOf(values);
    }

    public void update(String id, PendingAudio.State state, String detail) {
      values.replaceAll(
          v ->
              v.id().equals(id)
                  ? new PendingAudio(
                      v.id(),
                      v.locator(),
                      v.displayName(),
                      v.contentType(),
                      v.sourceId(),
                      v.recordedAtMs(),
                      v.byteLength(),
                      state,
                      detail)
                  : v);
    }

    public void complete(String id, String detail) {
      update(id, PendingAudio.State.UPLOADED, detail);
    }

    PendingAudio find(String id) {
      return values.stream().filter(v -> v.id().equals(id)).findFirst().orElseThrow();
    }
  }
}
