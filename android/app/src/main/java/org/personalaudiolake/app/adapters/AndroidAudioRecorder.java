package org.personalaudiolake.app.adapters;

import android.content.Context;
import android.media.MediaRecorder;
import android.os.Build;
import java.io.File;
import java.io.FileInputStream;
import java.io.FilterInputStream;
import java.io.IOException;
import java.time.Instant;
import java.time.ZoneId;
import java.time.format.DateTimeFormatter;
import java.util.UUID;
import org.personalaudiolake.app.model.AudioRecorder;

/** Android media leaf. Captures AAC audio to a disposable app-private file. */
public final class AndroidAudioRecorder implements AudioRecorder {
  /** Stay below the Mac ingest endpoint's 256 MiB hard limit. */
  private static final long MAX_BYTES = 250L * 1024L * 1024L;

  private static final int MAX_DURATION_MS = 4 * 60 * 60 * 1000;
  private static final DateTimeFormatter FILE_TIME =
      DateTimeFormatter.ofPattern("yyyy-MM-dd-HHmmss").withZone(ZoneId.systemDefault());

  private final Context context;
  private MediaRecorder recorder;
  private File output;
  private long recordedAtMs;

  public AndroidAudioRecorder(Context context) {
    this.context = context.getApplicationContext();
  }

  @Override
  public synchronized void start() throws Exception {
    if (recorder != null) throw new IllegalStateException("A recording is already in progress");
    File root = new File(context.getCacheDir(), "captures");
    if (!root.exists() && !root.mkdirs())
      throw new IOException("Could not create the recording cache");
    output = new File(root, UUID.randomUUID() + ".m4a");
    recordedAtMs = System.currentTimeMillis();
    MediaRecorder next =
        Build.VERSION.SDK_INT >= 31 ? new MediaRecorder(context) : new MediaRecorder();
    try {
      next.setAudioSource(MediaRecorder.AudioSource.MIC);
      next.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4);
      next.setAudioEncoder(MediaRecorder.AudioEncoder.AAC);
      next.setAudioEncodingBitRate(128_000);
      next.setAudioSamplingRate(44_100);
      next.setMaxFileSize(MAX_BYTES);
      next.setMaxDuration(MAX_DURATION_MS);
      next.setOutputFile(output.getAbsolutePath());
      next.prepare();
      next.start();
      recorder = next;
    } catch (Exception error) {
      next.release();
      deleteOutput();
      throw error;
    }
  }

  @Override
  public synchronized Capture stop() throws Exception {
    if (recorder == null) throw new IllegalStateException("No recording is in progress");
    MediaRecorder finished = recorder;
    recorder = null;
    try {
      finished.stop();
    } catch (RuntimeException error) {
      deleteOutput();
      throw new IllegalStateException("The recording was too short to save", error);
    } finally {
      finished.release();
    }
    File captured = output;
    output = null;
    if (captured == null || !captured.isFile() || captured.length() == 0) {
      if (captured != null) captured.delete();
      throw new IllegalStateException("Android did not produce an audio recording");
    }
    String name = "Recording-" + FILE_TIME.format(Instant.ofEpochMilli(recordedAtMs)) + ".m4a";
    return new Capture(new DeleteOnCloseInputStream(captured), name, "audio/mp4", recordedAtMs);
  }

  @Override
  public synchronized void cancel() {
    if (recorder != null) {
      try {
        recorder.stop();
      } catch (RuntimeException ignored) {
      }
      recorder.release();
      recorder = null;
    }
    deleteOutput();
  }

  private void deleteOutput() {
    if (output != null) output.delete();
    output = null;
  }

  private static final class DeleteOnCloseInputStream extends FilterInputStream {
    private final File file;

    private DeleteOnCloseInputStream(File file) throws IOException {
      super(new FileInputStream(file));
      this.file = file;
    }

    @Override
    public void close() throws IOException {
      try {
        super.close();
      } finally {
        file.delete();
      }
    }
  }
}
