fn main() {
    let client = reqwest::Client::new();
    let res = client.get("https://120.0.0.1/")
        .header("Authorization", "Bearer YOUR_TOKEN")
        .send()
        .await
        .unwrap();

    println!("Response: {:?}", res);
}