package org.personalaudiolake.app;

import android.content.Context;
import org.personalaudiolake.app.adapters.AndroidAudioRecorder;
import org.personalaudiolake.app.adapters.BluetoothRecordingSender;
import org.personalaudiolake.app.adapters.LocalLakeStore;

/** Android composition root; views never construct model or adapter implementations. */
public final class AppComposition {
  private AppComposition() {}

  public static MainViewModel create(Context context) {
    Context application = context.getApplicationContext();
    LocalLakeStore store = new LocalLakeStore(application);
    return new MainViewModel(
        new AndroidAudioRecorder(application), store, new BluetoothRecordingSender(application));
  }
}
