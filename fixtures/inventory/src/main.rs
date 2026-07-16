mod stock;

fn main() {
    let left = stock::reserve(3);
    println!("{left} units remain");
}
