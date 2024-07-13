use rusqlite::{params, Connection};

pub struct RtpLog {
    database: Connection,
}

impl RtpLog {
    pub fn new() -> anyhow::Result<Self> {
        let database = Connection::open("/tmp/rtp.db")?;
        let user_version =
            database.query_row_and_then("select * from pragma_user_version()", (), |row| {
                row.get::<usize, u64>(0)
            })?;

        if user_version == 0 {
            database.execute("CREATE TABLE rtp_packets (ssrc INTEGER, timestamp INTEGER, seq_no INTEGER, data BLOB) STRICT;", ())?;
            database.pragma_update(None, "user_version", 1)?;
        }

        Ok(Self { database })
    }

    pub fn log_packet(
        &self,
        ssrc: u32,
        timestamp: u32,
        seq_no: u16,
        data: &[u8],
    ) -> anyhow::Result<()> {
        self.database.execute(
            "INSERT INTO rtp_packets VALUES (?, ?, ?, ?)",
            params![ssrc, timestamp, seq_no, data],
        )?;

        Ok(())
    }
}
