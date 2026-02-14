use postgres::{Client, NoTls};
use machine_uid;
use sha2::{Sha256, Digest};
use ureq;


const DB_PARAMS: &str = "postgresql://postgres:admin@localhost/AdminHelper_db";

#[derive(Debug, Clone)]
pub enum AuthStatus {
    Success(String),       
    WrongCredentials,      
    HwidMismatch,          
    Banned(String),        
    DatabaseError(String), 
}

pub fn get_hwid() -> String {
    machine_uid::get().unwrap_or_else(|_| "UNKNOWN_HWID".to_string())
}

pub fn get_ip() -> String {
    // Теперь ureq::get сработает, так как мы добавили use ureq;
    match ureq::get("https://api.ipify.org").call() {
        Ok(resp) => resp.into_string().unwrap_or_else(|_| "0.0.0.0".to_string()),
        Err(_) => "0.0.0.0".to_string(),
    }
}

pub fn hash_password(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password);
    hex::encode(hasher.finalize())
}

pub fn try_login(username: &str, password: &str) -> AuthStatus {
    // 1. Подключаемся к БД
    let mut client = match Client::connect(DB_PARAMS, NoTls) {
        Ok(c) => c,
        Err(e) => return AuthStatus::DatabaseError(format!("Нет связи с БД: {}", e)),
    };

    let pass_hash = hash_password(password);
    let current_hwid = get_hwid();
    let current_ip = get_ip();

    // 2. Ищем пользователя
    let query = "SELECT id, password_hash, hwid, is_banned, admin_note, last_ip FROM users WHERE username = $1";
    
    if let Ok(row_opt) = client.query_opt(query, &[&username]) {
        if let Some(row) = row_opt {
            let db_pass: String = row.get("password_hash");
            let db_hwid: Option<String> = row.get("hwid");
            let is_banned: bool = row.get("is_banned");
            let last_ip: Option<String> = row.get("last_ip");
            let user_id: i32 = row.get("id");

            if db_pass != pass_hash {
                return AuthStatus::WrongCredentials;
            }

            if is_banned {
                let reason: Option<String> = row.get("admin_note");
                return AuthStatus::Banned(reason.unwrap_or_else(|| "Без причины".to_string()));
            }

            match db_hwid {
                None => {
                    let _ = client.execute("UPDATE users SET hwid = $1 WHERE id = $2", &[&current_hwid, &user_id]);
                },
                Some(saved_hwid) => {
                    if saved_hwid != current_hwid {
                        return AuthStatus::HwidMismatch; 
                    }
                }
            }

            if let Some(old_ip) = last_ip {
                if old_ip != current_ip {
                    let _ = client.execute(
                        "INSERT INTO ip_logs (user_id, old_ip, new_ip) VALUES ($1, $2, $3)",
                        &[&user_id, &old_ip, &current_ip]
                    );
                    let _ = client.execute("UPDATE users SET last_ip = $1 WHERE id = $2", &[&current_ip, &user_id]);
                }
            } else {
                let _ = client.execute("UPDATE users SET last_ip = $1 WHERE id = $2", &[&current_ip, &user_id]);
            }

            return AuthStatus::Success(username.to_string());

        } else {
            return AuthStatus::WrongCredentials; 
        }
    } else {
        return AuthStatus::DatabaseError("Ошибка SQL запроса".to_string());
    }
}