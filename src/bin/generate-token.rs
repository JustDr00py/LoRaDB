use loradb::security::jwt::{Claims, JwtService};
use std::env;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <username> [jwt_secret]", args[0]);
        eprintln!("\nIf jwt_secret is not provided, it will be read from LORADB_API_JWT_SECRET env var");
        std::process::exit(1);
    }

    let username = &args[1];

    // Get JWT secret from arg or env
    let jwt_secret = if args.len() >= 3 {
        args[2].clone()
    } else {
        env::var("LORADB_API_JWT_SECRET")
            .expect("LORADB_API_JWT_SECRET environment variable not set")
    };

    // Create JWT service
    let jwt_service = JwtService::new(&jwt_secret)?;

    // Create claims
    let claims = Claims::new(username.to_string());

    // Generate token
    let token = jwt_service.generate_token(claims)?;

    println!("Generated JWT token for user '{}':", username);
    println!("\n{}\n", token);
    println!("Use this token in API requests:");
    println!("curl -H 'Authorization: Bearer {}' https://your-domain.com/devices", token);

    Ok(())
}
