extern "C" {
        fn get_num(x: i32) -> i32;
        fn printer(x: i32) -> i32;
        fn get_dbl(x: i32) -> f64;
}

//extern "C" { pub fn get_num () -> :: std :: os :: raw :: c_int ; }

fn main() {
        let x;
        unsafe {
                    //println!("{}", get_num());
//                    printer(x);
                    x = get_num(1);
                    let y = get_dbl(x);
                    println!("{}", x);
                    println!("{}", y);
        }
        println!("x: {:?}", x);
        println!("rjeiworjweio");
}
