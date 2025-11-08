// After line 27, add:
let (tx_write_update, mut rx_write_update) = mpsc::channel(1);
let (tx_read_update, mut rx_read_update) = mpsc::channel(1);

// In sender task, use select! to listen for both rx_outbound and rx_write_update:
use tokio::select;
loop {
    select! {
        msg = rx_outbound.recv() => {
            // handle message
        }
        new_write = rx_write_update.recv() => {
            if let Some(w) = new_write {
                write = w;
            }
        }
    }
}

// In reader task, use select! similarly:
loop {
    select! {
        msg = read.next() => {
            // handle message
        }
        new_read = rx_read_update.recv() => {
            if let Some(r) = new_read {
                read = r;
            }
        }
    }
}

// On reconnection (line 74-76):
let (new_write, new_read) = new_ws_stream.split();
tx_write_update.send(new_write).await?;
tx_read_update.send(new_read).await?;