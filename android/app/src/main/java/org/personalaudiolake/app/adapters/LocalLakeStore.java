package org.personalaudiolake.app.adapters;

import android.content.ContentValues;
import android.content.Context;
import android.database.Cursor;
import android.database.sqlite.SQLiteDatabase;
import android.database.sqlite.SQLiteOpenHelper;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;
import org.personalaudiolake.app.model.PendingAudio;
import org.personalaudiolake.app.model.PendingAudioStore;

/** App-private durable leaf for captured audio and its Bluetooth transfer queue. */
public final class LocalLakeStore extends SQLiteOpenHelper
    implements PendingAudioStore<InputStream> {
  private static final int DATABASE_VERSION = 4;
  private final File audioRoot;

  public LocalLakeStore(Context context) {
    this(context, "personal-audio-lake.db", new File(context.getFilesDir(), "audio"));
  }

  LocalLakeStore(Context context, String databaseName, File audioRoot) {
    super(context, databaseName, null, DATABASE_VERSION);
    this.audioRoot = audioRoot;
  }

  @Override
  public void onCreate(SQLiteDatabase db) {
    db.execSQL(
        "CREATE TABLE pending_audio (id TEXT PRIMARY KEY, locator TEXT NOT NULL, display_name TEXT"
            + " NOT NULL, content_type TEXT NOT NULL, source_id TEXT NOT NULL, recorded_at_ms"
            + " INTEGER NOT NULL, byte_length INTEGER NOT NULL, state TEXT NOT NULL, detail TEXT"
            + " NOT NULL)");
  }

  @Override
  public void onUpgrade(SQLiteDatabase db, int oldVersion, int newVersion) {
    if (oldVersion < 3) {
      db.execSQL("DROP TABLE IF EXISTS library");
      db.execSQL("DROP TABLE IF EXISTS effect_receipts");
    }
    if (oldVersion < 4) db.execSQL("DROP TABLE IF EXISTS settings");
  }

  @Override
  public synchronized PendingAudio stage(
      InputStream input, String displayName, String contentType, long recordedAtMs)
      throws Exception {
    String extension = extension(contentType);
    if (extension == null) extension = extensionFromName(displayName);
    if (extension == null)
      throw new IllegalArgumentException("Unsupported audio type: " + contentType);
    if (!audioRoot.exists() && !audioRoot.mkdirs()) {
      throw new IllegalStateException("Could not create the private recording directory");
    }
    String id = UUID.randomUUID().toString();
    File destination = new File(audioRoot, id + "." + extension);
    File partial = new File(audioRoot, id + ".partial");
    long bytes = 0;
    try (InputStream source = input;
        FileOutputStream output = new FileOutputStream(partial)) {
      byte[] buffer = new byte[128 * 1024];
      for (int count; (count = source.read(buffer)) != -1; ) {
        output.write(buffer, 0, count);
        bytes += count;
      }
      output.getFD().sync();
    } catch (Exception error) {
      partial.delete();
      throw error;
    }
    if (bytes == 0) {
      partial.delete();
      throw new IllegalArgumentException("The recording is empty");
    }
    if (!partial.renameTo(destination)) {
      partial.delete();
      throw new IllegalStateException("Could not commit the private recording");
    }
    PendingAudio value =
        new PendingAudio(
            id,
            destination.getAbsolutePath(),
            safeName(displayName),
            canonicalType(contentType, extension),
            "android-recorder",
            recordedAtMs,
            bytes,
            PendingAudio.State.QUEUED,
            "Waiting to send");
    try {
      getWritableDatabase().insertOrThrow("pending_audio", null, audioValues(value));
    } catch (RuntimeException error) {
      destination.delete();
      throw error;
    }
    return value;
  }

  @Override
  public synchronized List<PendingAudio> awaitingUpload() {
    List<PendingAudio> result = new ArrayList<>();
    try (Cursor rows =
        getReadableDatabase()
            .query(
                "pending_audio",
                null,
                "state IN (?, ?, ?)",
                new String[] {"QUEUED", "FAILED", "UPLOADING"},
                null,
                null,
                "recorded_at_ms ASC")) {
      while (rows.moveToNext()) result.add(audio(rows));
    }
    return result;
  }

  @Override
  public synchronized List<PendingAudio> recent() {
    List<PendingAudio> result = new ArrayList<>();
    try (Cursor rows =
        getReadableDatabase()
            .query("pending_audio", null, null, null, null, null, "recorded_at_ms DESC", "50")) {
      while (rows.moveToNext()) result.add(audio(rows));
    }
    return result;
  }

  @Override
  public synchronized void update(String id, PendingAudio.State state, String detail) {
    ContentValues values = new ContentValues();
    values.put("state", state.name());
    values.put("detail", detail == null ? "" : detail);
    getWritableDatabase().update("pending_audio", values, "id = ?", new String[] {id});
  }

  @Override
  public synchronized void complete(String id, String detail) {
    PendingAudio value = null;
    try (Cursor row =
        getReadableDatabase()
            .query("pending_audio", null, "id = ?", new String[] {id}, null, null, null)) {
      if (row.moveToFirst()) value = audio(row);
    }
    update(id, PendingAudio.State.UPLOADED, detail);
    if (value != null) new File(value.locator()).delete();
  }

  private static ContentValues audioValues(PendingAudio value) {
    ContentValues values = new ContentValues();
    values.put("id", value.id());
    values.put("locator", value.locator());
    values.put("display_name", value.displayName());
    values.put("content_type", value.contentType());
    values.put("source_id", value.sourceId());
    values.put("recorded_at_ms", value.recordedAtMs());
    values.put("byte_length", value.byteLength());
    values.put("state", value.state().name());
    values.put("detail", value.detail());
    return values;
  }

  private static PendingAudio audio(Cursor row) {
    return new PendingAudio(
        text(row, "id"),
        text(row, "locator"),
        text(row, "display_name"),
        text(row, "content_type"),
        text(row, "source_id"),
        number(row, "recorded_at_ms"),
        number(row, "byte_length"),
        PendingAudio.State.valueOf(text(row, "state")),
        text(row, "detail"));
  }

  private static String text(Cursor row, String column) {
    return row.getString(row.getColumnIndexOrThrow(column));
  }

  private static long number(Cursor row, String column) {
    return row.getLong(row.getColumnIndexOrThrow(column));
  }

  private static String safeName(String name) {
    return name == null || name.isBlank() ? "Recording" : name.replaceAll("[\\r\\n]", " ");
  }

  private static String normalizedType(String type) {
    return type == null ? "" : type.split(";", 2)[0].trim().toLowerCase(java.util.Locale.ROOT);
  }

  private static String extension(String type) {
    return switch (normalizedType(type)) {
      case "audio/mpeg" -> "mp3";
      case "audio/mp4", "audio/x-m4a" -> "m4a";
      case "audio/wav", "audio/x-wav" -> "wav";
      case "audio/aac" -> "aac";
      case "audio/flac", "audio/x-flac" -> "flac";
      case "audio/ogg" -> "ogg";
      case "audio/opus" -> "opus";
      default -> null;
    };
  }

  private static String extensionFromName(String name) {
    if (name == null || !name.contains(".")) return null;
    String value = name.substring(name.lastIndexOf('.') + 1).toLowerCase(java.util.Locale.ROOT);
    return switch (value) {
      case "mp3", "m4a", "wav", "aac", "flac", "ogg", "opus" -> value;
      default -> null;
    };
  }

  private static String canonicalType(String type, String extension) {
    if (extension(type) != null) return normalizedType(type);
    return switch (extension) {
      case "mp3" -> "audio/mpeg";
      case "m4a" -> "audio/mp4";
      case "wav" -> "audio/wav";
      case "aac" -> "audio/aac";
      case "flac" -> "audio/flac";
      case "ogg" -> "audio/ogg";
      case "opus" -> "audio/opus";
      default -> throw new IllegalArgumentException("Unsupported audio extension");
    };
  }
}
