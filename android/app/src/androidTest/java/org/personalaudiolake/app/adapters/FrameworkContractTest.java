package org.personalaudiolake.app.adapters;

import android.content.Context;
import android.test.InstrumentationTestCase;
import java.io.ByteArrayInputStream;
import java.io.File;
import org.personalaudiolake.app.model.PendingAudio;

public final class FrameworkContractTest extends InstrumentationTestCase {
  public void testPrivateQueuePersistsAndCompletedAudioIsRemoved() throws Exception {
    Context context = getInstrumentation().getTargetContext();
    String database = "captains-log-bluetooth-test.db";
    File root = new File(context.getCacheDir(), "captains-log-bluetooth-test");
    context.deleteDatabase(database);
    delete(root);
    LocalLakeStore store = new LocalLakeStore(context, database, root);
    PendingAudio audio =
        store.stage(new ByteArrayInputStream(new byte[] {1, 2, 3}), "a.m4a", "audio/mp4", 10);
    assertTrue(new File(audio.locator()).isFile());
    store.close();
    store = new LocalLakeStore(context, database, root);
    assertEquals(1, store.awaitingUpload().size());
    store.complete(audio.id(), "sent");
    assertEquals(0, store.awaitingUpload().size());
    assertFalse(new File(audio.locator()).exists());
    store.close();
    context.deleteDatabase(database);
    delete(root);
  }

  private static void delete(File value) {
    File[] children = value.listFiles();
    if (children != null) for (File child : children) delete(child);
    if (value.exists()) value.delete();
  }
}
