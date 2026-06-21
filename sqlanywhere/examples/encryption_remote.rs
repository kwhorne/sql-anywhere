// Example: Connecting to an encrypted Elyra Cloud database
//
// This example shows how to connect to a Elyra Cloud database with
// remote encryption using the rust driver.
//
// Documentation of encrypted databases - https://docs.elyracode.com/cloud/encryption
//
// Usage:
//
//  export SQLANYWHERE_DB_URL="sqlanywhere://your-db.aws-us-east-2.elyra.io"
//  export SQLANYWHERE_AUTH_TOKEN="your-token"
//  export SQLANYWHERE_ENCRYPTION_KEY="encryption key in base 64 encoded format"
//  cargo run --example encryption_remote
//
// The encryption key must be encoded in base64 format.

use sqlanywhere::{params, Builder};
use sqlanywhere::{EncryptionContext, EncryptionKey};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // The remote DB URL to use.
    let db_url = std::env::var("SQLANYWHERE_DB_URL").unwrap();

    // The authentication token for the remote db
    let auth_token = std::env::var("SQLANYWHERE_AUTH_TOKEN").unwrap_or("".to_string());

    // Optional encryption key for the database, if provided.
    let encryption = if let Ok(key) = std::env::var("SQLANYWHERE_ENCRYPTION_KEY") {
        Some(EncryptionContext {
            key: EncryptionKey::Base64Encoded(key),
        })
    } else {
        None
    };

    let mut db_builder = Builder::new_remote(db_url, auth_token);
    if let Some(enc) = encryption {
        db_builder = db_builder.remote_encryption(enc);
    }
    let db = db_builder.build().await.unwrap();
    let conn = db.connect().unwrap();

    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS guest_book_entries (
            text TEXT
        )"#,
        (),
    )
    .await
    .unwrap();

    let mut input = String::new();
    println!("Please write your entry to the guestbook:");
    match std::io::stdin().read_line(&mut input) {
        Ok(_) => {
            println!("You entered: {}", input);
            let params = params![input.as_str()];
            conn.execute("INSERT INTO guest_book_entries (text) VALUES (?)", params)
                .await
                .unwrap();
        }
        Err(error) => {
            eprintln!("Error reading input: {}", error);
        }
    }
    let mut results = conn
        .query("SELECT * FROM guest_book_entries", ())
        .await
        .unwrap();
    println!("Guest book entries:");
    while let Some(row) = results.next().await.unwrap() {
        let text: String = row.get(0).unwrap();
        println!("  {}", text);
    }
}
