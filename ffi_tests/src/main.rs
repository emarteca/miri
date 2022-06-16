extern "C" {
        fn get_num() -> i32;
        fn printer(x: i32) -> i32;
}

//extern "C" { pub fn get_num () -> :: std :: os :: raw :: c_int ; }

fn main() {
        let x;
        unsafe {
                    //println!("{}", get_num());
//                    printer(x);
                    x = get_num();
                    println!("{}", x);
        }
        println!("x: {:?}", x);
        println!("rjeiworjweio");
}
