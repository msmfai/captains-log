package org.personalaudiolake.app.model;

import java.util.List;

/** Sends already-durable recordings without exposing a platform transport to the ViewModel. */
public interface RecordingSender {
  record Receipt(String audioId, String detail) {}

  List<Receipt> send(List<PendingAudio> recordings) throws Exception;

  void cancel();
}
