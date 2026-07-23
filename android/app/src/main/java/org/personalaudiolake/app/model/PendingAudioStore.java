package org.personalaudiolake.app.model;

import java.util.List;

public interface PendingAudioStore<Input> {
  PendingAudio stage(Input input, String displayName, String contentType, long recordedAtMs)
      throws Exception;

  List<PendingAudio> awaitingUpload();

  List<PendingAudio> recent();

  void update(String id, PendingAudio.State state, String detail);

  void complete(String id, String detail);
}
