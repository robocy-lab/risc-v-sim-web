use anyhow::Result;
use chrono::Utc;
use risc_v_sim_web::database::{DatabaseService, SubmissionRecord, SubmissionStatus};
use serde_json;
use std::env;
use ulid::Ulid;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize database connection
    let db_service = DatabaseService::new().await?;

    // Test user IDs
    let test_user_ids = vec![
        75020830i64, // miko089
        98765432i64, // testuser1
        55566677i64, // testuser2
        11122233i64, // testuser3
    ];

    // Create sample submissions for each user
    for (index, user_id) in test_user_ids.iter().enumerate() {
        println!("Creating submissions for user ID: {}", user_id);

        // Create 5-10 submissions per user
        let num_submissions = if index == 0 {
            8
        } else {
            rand::random::<usize>() % 6 + 3
        };

        for i in 0..num_submissions {
            let uuid = Ulid::new().to_string();
            let status = match i % 3 {
                0 => SubmissionStatus::Completed,
                1 => SubmissionStatus::InProgress,
                _ => SubmissionStatus::Awaits,
            };

            // Vary the created_at times
            let hours_ago = (i * 2 + rand::random::<usize>() % 24) as i64;
            let created_at = Utc::now() - chrono::Duration::hours(hours_ago);
            let updated_at = if matches!(status, SubmissionStatus::Completed) {
                created_at + chrono::Duration::minutes(rand::random::<i64>().abs() % 120)
            } else {
                created_at
            };

            let submission = SubmissionRecord {
                id: None,
                uuid: uuid.clone(),
                user_id: *user_id,
                status,
                created_at,
                updated_at,
            };

            // Insert into database
            let inserted_id = db_service.create_submission(submission).await?;
            println!("  Created submission {} with ID: {}", uuid, inserted_id);

            create_submission_files(&uuid, i).await?;
        }
    }

    println!("Database population completed!");
    Ok(())
}

async fn create_submission_files(uuid: &str, index: usize) -> Result<()> {
    let submissions_dir =
        env::var("SUBMISSIONS_FOLDER").unwrap_or_else(|_| "submission".to_string());
    let submission_path = format!("{}/{}", submissions_dir, uuid);

    // Create directory
    tokio::fs::create_dir_all(&submission_path).await?;

    // Create sample assembly code
    let sample_codes = vec![
        r#"# Simple addition program
        .text
        .globl _start
        
    _start:
        li x5, 10      # Load 10 into x5
        li x6, 20      # Load 20 into x6
        add x7, x5, x6 # Add x5 and x6, store in x7
        ebreak         # End program"#,
        r#"# Loop example
        .text
        .globl _start
        
    _start:
        li x5, 0       # Initialize counter
        li x6, 5       # Loop count
        
    loop:
        addi x5, x5, 1 # Increment counter
        bne x5, x6, loop # Continue if not equal
        ebreak"#,
        r#"# Memory operations
        .text
        .globl _start
        
    _start:
        la x10, data   # Load address of data
        li x11, 42     # Value to store
        sw x11, 0(x10) # Store value
        lw x12, 0(x10) # Load value back
        ebreak
        
    .data
    data: .word 0"#,
        r#"# Fibonacci sequence
        .text
        .globl _start
        
    _start:
        li x5, 0       # F(0)
        li x6, 1       # F(1)
        li x7, 10      # Count
        
    fib_loop:
        add x8, x5, x6 # Next Fibonacci number
        mv x5, x6      # Shift
        mv x6, x8
        addi x7, x7, -1 # Decrement counter
        bnez x7, fib_loop
        ebreak"#,
        r#"# Simple arithmetic
        .text
        .globl _start
        
    _start:
        li x5, 100
        li x6, 25
        sub x7, x5, x6 # 100 - 25 = 75
        li x8, 3
        mul x9, x7, x8 # 75 * 3 = 225
        ebreak"#,
    ];

    let code_index = index % sample_codes.len();
    let assembly_code = sample_codes[code_index];

    let input_file = format!("{}/input.s", submission_path);
    tokio::fs::write(&input_file, assembly_code).await?;

    if index % 3 == 0 {
        let simulation_result = create_sample_simulation_result(&assembly_code, index);
        let result_file = format!("{}/simulation.json", submission_path);
        tokio::fs::write(result_file, simulation_result).await?;
    }

    Ok(())
}

fn create_sample_simulation_result(assembly_code: &str, index: usize) -> String {
    let ulid = Ulid::new();
    let steps = if index % 3 == 0 {
        vec![
            serde_json::json!({
                "instruction": {
                    "mnemonic": "li",
                    "obj": {"Li": [5, 10]}
                },
                "old_registers": {
                    "pc": 1342177280,
                    "storage": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
                },
                "new_registers": {
                    "pc": 1342177284,
                    "storage": [0, 0, 0, 0, 0, 10, 0, 0, 0, 0]
                }
            }),
            serde_json::json!({
                "instruction": {
                    "mnemonic": "li",
                    "obj": {"Li": [6, 20]}
                },
                "old_registers": {
                    "pc": 1342177284,
                    "storage": [0, 0, 0, 0, 0, 10, 0, 0, 0, 0]
                },
                "new_registers": {
                    "pc": 1342177288,
                    "storage": [0, 0, 0, 0, 0, 10, 20, 0, 0, 0]
                }
            }),
            serde_json::json!({
                "instruction": {
                    "mnemonic": "add",
                    "obj": {"Add": [7, 5, 6]}
                },
                "old_registers": {
                    "pc": 1342177288,
                    "storage": [0, 0, 0, 0, 0, 10, 20, 0, 0, 0]
                },
                "new_registers": {
                    "pc": 1342177292,
                    "storage": [0, 0, 0, 0, 0, 10, 20, 30, 0, 0]
                }
            }),
        ]
    } else {
        vec![]
    };

    let result = serde_json::json!({
        "ulid": ulid.to_string(),
        "ticks": 10,
        "code": assembly_code,
        "steps": steps,
        "final_registers": {
            "pc": 1342177292,
            "storage": [0, 0, 0, 0, 0, 10, 20, 30, 0, 0]
        }
    });

    result.to_string()
}
