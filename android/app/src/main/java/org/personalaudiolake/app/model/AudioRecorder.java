package org.personalaudiolake.app.model;

import java.io.InputStream;

/** Platform-neutral recording source. Android media APIs live behind this port. */
public interface AudioRecorder {
  record Capture(InputStream bytes, String displayName, String contentType, long recordedAtMs)
      implements AutoCloseable {
    @Override
    public void close() throws Exception {
      bytes.close();
    }
  }

  void start() throws Exception;

  Capture stop() throws Exception;

  void cancel();
}
