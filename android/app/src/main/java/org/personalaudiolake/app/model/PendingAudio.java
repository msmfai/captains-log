package org.personalaudiolake.app.model;

public record PendingAudio(
    String id,
    String locator,
    String displayName,
    String contentType,
    String sourceId,
    long recordedAtMs,
    long byteLength,
    State state,
    String detail) {
  public enum State {
    QUEUED,
    UPLOADING,
    UPLOADED,
    FAILED
  }
}
